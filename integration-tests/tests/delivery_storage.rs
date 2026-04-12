//! Integration tests for `DeliveryStorage` against a live Postgres testcontainer.
//! Run with: `cargo nextest run -p hookbox-integration-tests -- delivery_storage`

#![expect(
    clippy::expect_used,
    reason = "expect/unwrap/panic are acceptable in test code"
)]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;

use chrono::Utc;
use hookbox_postgres::{DeliveryStorage, PostgresStorage};
use uuid::Uuid;

async fn seed_receipt(pool: &sqlx::PgPool, receipt_id: Uuid) {
    sqlx::query(
        "INSERT INTO webhook_receipts (
            receipt_id, provider_name, dedupe_key, payload_hash, raw_body,
            raw_headers, verification_status, processing_state, emit_count,
            received_at, metadata
        ) VALUES ($1, 'test', $2, 'h', $3, '{}'::jsonb, 'verified', 'stored', 0, now(), '{}'::jsonb)",
    )
    .bind(receipt_id)
    .bind(format!("test:{receipt_id}"))
    .bind(b"{}".as_slice())
    .execute(pool)
    .await
    .expect("seed receipt");
}

#[expect(
    clippy::too_many_arguments,
    reason = "seed helper needs all delivery fields"
)]
async fn seed_delivery(
    pool: &sqlx::PgPool,
    delivery_id: Uuid,
    receipt_id: Uuid,
    emitter: &str,
    state: &str,
    immutable: bool,
    next_attempt_at: chrono::DateTime<chrono::Utc>,
    last_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    sqlx::query(
        "INSERT INTO webhook_deliveries (
            delivery_id, receipt_id, emitter_name, state, attempt_count,
            next_attempt_at, last_attempt_at, immutable, created_at
        ) VALUES ($1, $2, $3, $4, 0, $5, $6, $7, now())",
    )
    .bind(delivery_id)
    .bind(receipt_id)
    .bind(emitter)
    .bind(state)
    .bind(next_attempt_at)
    .bind(last_attempt_at)
    .bind(immutable)
    .execute(pool)
    .await
    .expect("seed delivery");
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn claim_pending_returns_only_eligible_rows(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;

    // Eligible: pending, next_attempt_at = now - 1s
    let eligible_id = Uuid::new_v4();
    seed_delivery(
        &pool,
        eligible_id,
        receipt_id,
        "test-emitter",
        "pending",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        None,
    )
    .await;

    // Not yet eligible: pending, next_attempt_at = far future
    let future_id = Uuid::new_v4();
    seed_delivery(
        &pool,
        future_id,
        receipt_id,
        "test-emitter",
        "pending",
        false,
        Utc::now() + chrono::Duration::hours(1),
        None,
    )
    .await;

    // Already in_flight: must not be claimed again
    let in_flight_id = Uuid::new_v4();
    seed_delivery(
        &pool,
        in_flight_id,
        receipt_id,
        "test-emitter",
        "in_flight",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    let claimed = storage
        .claim_pending("test-emitter", 10)
        .await
        .expect("claim_pending");

    assert_eq!(
        claimed.len(),
        1,
        "exactly one eligible row should be claimed"
    );
    assert_eq!(
        claimed[0].0.delivery_id.0, eligible_id,
        "wrong delivery claimed"
    );
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn two_concurrent_claims_see_disjoint_sets(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;

    // Seed 10 eligible pending rows
    let mut all_ids = Vec::new();
    for _ in 0..10 {
        let delivery_id = Uuid::new_v4();
        all_ids.push(delivery_id);
        seed_delivery(
            &pool,
            delivery_id,
            receipt_id,
            "test-emitter",
            "pending",
            false,
            Utc::now() - chrono::Duration::seconds(1),
            None,
        )
        .await;
    }

    // Two concurrent claims of 5 each
    let (batch_a, batch_b) = tokio::join!(
        storage.claim_pending("test-emitter", 5),
        storage.claim_pending("test-emitter", 5),
    );
    let batch_a = batch_a.expect("claim batch_a");
    let batch_b = batch_b.expect("claim batch_b");

    let ids_a: HashSet<Uuid> = batch_a.iter().map(|(d, _)| d.delivery_id.0).collect();
    let ids_b: HashSet<Uuid> = batch_b.iter().map(|(d, _)| d.delivery_id.0).collect();

    // No delivery_id must appear in both sets
    let overlap: HashSet<_> = ids_a.intersection(&ids_b).collect();
    assert!(
        overlap.is_empty(),
        "concurrent claims must be disjoint; overlap: {overlap:?}"
    );
    assert_eq!(
        ids_a.len() + ids_b.len(),
        10,
        "together they must cover all 10 rows"
    );
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn claim_pending_skips_immutable_rows(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;

    let delivery_id = Uuid::new_v4();
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "pending",
        true,
        Utc::now() - chrono::Duration::seconds(1),
        None,
    )
    .await;

    let claimed = storage
        .claim_pending("test-emitter", 10)
        .await
        .expect("claim_pending");
    assert!(claimed.is_empty(), "immutable rows must never be claimed");
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn mark_emitted_sets_terminal_state(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "in_flight",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    storage
        .mark_emitted(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("mark_emitted");

    let (row, _receipt) = storage
        .get_delivery(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("get_delivery")
        .expect("row must exist");
    assert_eq!(row.state, hookbox::state::DeliveryState::Emitted);
    assert!(row.emitted_at.is_some(), "emitted_at must be set");
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn mark_failed_increments_attempt_and_sets_next_attempt(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "in_flight",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    let next_attempt_count = 1_i32;
    let next_at = Utc::now() + chrono::Duration::seconds(30);
    storage
        .mark_failed(
            hookbox::state::DeliveryId(delivery_id),
            next_attempt_count,
            next_at,
            "downstream error",
        )
        .await
        .expect("mark_failed");

    let (row, _receipt) = storage
        .get_delivery(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("get_delivery")
        .expect("row must exist");
    assert_eq!(row.state, hookbox::state::DeliveryState::Failed);
    assert_eq!(row.attempt_count, next_attempt_count);
    assert!(
        row.next_attempt_at >= next_at - chrono::Duration::seconds(1),
        "next_attempt_at must be near the requested value"
    );
    assert_eq!(row.last_error.as_deref(), Some("downstream error"));
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn mark_dead_lettered_sets_terminal_state(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "in_flight",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    storage
        .mark_dead_lettered(
            hookbox::state::DeliveryId(delivery_id),
            "max retries exceeded",
        )
        .await
        .expect("mark_dead_lettered");

    let (row, _receipt) = storage
        .get_delivery(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("get_delivery")
        .expect("row must exist");
    assert_eq!(row.state, hookbox::state::DeliveryState::DeadLettered);
    assert_eq!(row.last_error.as_deref(), Some("max retries exceeded"));
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn insert_replay_creates_fresh_row_leaving_original(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "dead_lettered",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    let new_id = storage
        .insert_replay(hookbox::state::ReceiptId(receipt_id), "test-emitter")
        .await
        .expect("insert_replay");

    // Original row is unchanged
    let (original, _receipt) = storage
        .get_delivery(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("get_delivery")
        .expect("original must exist");
    assert_eq!(original.state, hookbox::state::DeliveryState::DeadLettered);

    // New row is pending with attempt_count = 0
    let (new_row, _receipt2) = storage
        .get_delivery(new_id)
        .await
        .expect("get_delivery new")
        .expect("new row must exist");
    assert_eq!(new_row.state, hookbox::state::DeliveryState::Pending);
    assert_eq!(new_row.attempt_count, 0);
    assert_ne!(
        new_row.delivery_id.0, delivery_id,
        "must be a different row"
    );
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn reclaim_expired_promotes_stale_in_flight_to_failed(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    // last_attempt_at = 10 minutes ago → expired under a 60-second lease
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "in_flight",
        false,
        Utc::now() - chrono::Duration::minutes(10),
        Some(Utc::now() - chrono::Duration::minutes(10)),
    )
    .await;

    let reclaimed = storage
        .reclaim_expired("test-emitter", std::time::Duration::from_secs(60))
        .await
        .expect("reclaim_expired");
    assert_eq!(reclaimed, 1, "one row should be reclaimed");

    let (row, _receipt) = storage
        .get_delivery(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("get_delivery")
        .expect("row must exist");
    assert_eq!(row.state, hookbox::state::DeliveryState::Failed);
    assert!(
        row.last_error
            .as_deref()
            .unwrap_or("")
            .contains("[reclaimed: lease expired]"),
        "last_error must contain the reclaim marker; got: {:?}",
        row.last_error
    );
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn reclaim_expired_does_not_touch_active_in_flight(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    let delivery_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;
    // last_attempt_at = just now → well within the 60-second lease
    seed_delivery(
        &pool,
        delivery_id,
        receipt_id,
        "test-emitter",
        "in_flight",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    let reclaimed = storage
        .reclaim_expired("test-emitter", std::time::Duration::from_secs(60))
        .await
        .expect("reclaim_expired");
    assert_eq!(reclaimed, 0, "active in_flight row must not be reclaimed");

    let (row, _receipt) = storage
        .get_delivery(hookbox::state::DeliveryId(delivery_id))
        .await
        .expect("get_delivery")
        .expect("row must exist");
    assert_eq!(
        row.state,
        hookbox::state::DeliveryState::InFlight,
        "state must remain in_flight"
    );
}

#[sqlx::test(migrations = "../crates/hookbox-postgres/migrations")]
async fn count_dlq_and_pending_correctness(pool: sqlx::PgPool) {
    let storage = PostgresStorage::new(pool.clone());
    let receipt_id = Uuid::new_v4();
    seed_receipt(&pool, receipt_id).await;

    // Seed 2 dead_lettered, 3 pending, 1 emitted (should not count toward either)
    for _ in 0..2 {
        seed_delivery(
            &pool,
            Uuid::new_v4(),
            receipt_id,
            "test-emitter",
            "dead_lettered",
            false,
            Utc::now() - chrono::Duration::seconds(1),
            Some(Utc::now()),
        )
        .await;
    }
    for _ in 0..3 {
        seed_delivery(
            &pool,
            Uuid::new_v4(),
            receipt_id,
            "test-emitter",
            "pending",
            false,
            Utc::now() - chrono::Duration::seconds(1),
            None,
        )
        .await;
    }
    seed_delivery(
        &pool,
        Uuid::new_v4(),
        receipt_id,
        "test-emitter",
        "emitted",
        false,
        Utc::now() - chrono::Duration::seconds(1),
        Some(Utc::now()),
    )
    .await;

    let dlq_depth = storage.count_dlq("test-emitter").await.expect("count_dlq");
    let pending_count = storage
        .count_pending("test-emitter")
        .await
        .expect("count_pending");

    assert_eq!(dlq_depth, 2, "expected 2 dead_lettered rows");
    assert_eq!(pending_count, 3, "expected 3 pending rows");
}
