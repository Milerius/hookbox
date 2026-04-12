//! Verifies migration 0002 creates the table, indexes, and backfills correctly.
//!
//! Backfill tests use a manual schema setup: migration 0001 is applied first,
//! receipts are seeded, then migration 0002 SQL is executed — mirroring what
//! happens on a real database that already has data when this migration runs.

#![expect(
    clippy::expect_used,
    reason = "expect/unwrap/panic are acceptable in test code"
)]
#![allow(clippy::unwrap_used, clippy::panic)]

use sqlx::PgPool;
use uuid::Uuid;

// ── SQL text for each migration ───────────────────────────────────────────────

const MIGRATION_0001: &str =
    include_str!("../../crates/hookbox-postgres/migrations/0001_create_webhook_receipts.sql");
const MIGRATION_0002: &str = include_str!(
    "../../crates/hookbox-postgres/migrations/0002_create_webhook_deliveries.sql"
);

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Create a fresh schema with only migration 0001 applied.
/// Each call creates an isolated schema inside the provided pool.
async fn setup_pre_migration(pool: &PgPool) {
    // Drop any leftover tables so each test starts clean.
    sqlx::query("DROP TABLE IF EXISTS webhook_deliveries CASCADE")
        .execute(pool)
        .await
        .expect("drop webhook_deliveries");
    sqlx::query("DROP TABLE IF EXISTS webhook_receipts CASCADE")
        .execute(pool)
        .await
        .expect("drop webhook_receipts");
    // Apply migration 0001.
    sqlx::raw_sql(MIGRATION_0001)
        .execute(pool)
        .await
        .expect("apply migration 0001");
}

/// Apply migration 0002 against an already-set-up pool.
async fn apply_migration_0002(pool: &PgPool) {
    sqlx::raw_sql(MIGRATION_0002)
        .execute(pool)
        .await
        .expect("apply migration 0002");
}

/// Insert a minimal receipt row and return its `receipt_id`.
/// Uses a unique `dedupe_key` per call so tests don't collide.
async fn seed_receipt(pool: &PgPool, state: &str) -> Uuid {
    let id = Uuid::new_v4();
    let dedupe_key = Uuid::new_v4().to_string();
    let empty_body: Vec<u8> = Vec::new();
    sqlx::query!(
        r#"
        INSERT INTO webhook_receipts
            (receipt_id, provider_name, dedupe_key, payload_hash,
             raw_body, raw_headers, verification_status,
             processing_state, received_at)
        VALUES
            ($1, 'test-provider', $2, 'deadbeef',
             $3, '{}'::jsonb, 'verified',
             $4, now())
        "#,
        id,
        dedupe_key,
        &empty_body,
        state,
    )
    .execute(pool)
    .await
    .expect("seed receipt");
    id
}

// ── Backfill tests (manual schema setup) ─────────────────────────────────────

/// The migration backfills exactly one `webhook_deliveries` row per receipt.
#[sqlx::test]
async fn migration_backfills_one_row_per_receipt(pool: PgPool) {
    setup_pre_migration(&pool).await;

    let emitted_id = seed_receipt(&pool, "emitted").await;
    let failed_id = seed_receipt(&pool, "emit_failed").await;
    let stored_id = seed_receipt(&pool, "stored").await;
    let dead_id = seed_receipt(&pool, "dead_lettered").await;

    apply_migration_0002(&pool).await;

    let count_for = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar!(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE receipt_id = $1",
                id
            )
            .fetch_one(&pool)
            .await
            .unwrap()
            .unwrap_or(0)
        }
    };

    assert_eq!(count_for(emitted_id).await, 1, "emitted");
    assert_eq!(count_for(failed_id).await, 1, "emit_failed");
    assert_eq!(count_for(stored_id).await, 1, "stored");
    assert_eq!(count_for(dead_id).await, 1, "dead_lettered");
}

/// All backfilled rows must have `immutable = true`.
#[sqlx::test]
async fn migration_backfill_rows_are_immutable(pool: PgPool) {
    setup_pre_migration(&pool).await;
    let _ = seed_receipt(&pool, "emitted").await;
    apply_migration_0002(&pool).await;

    let rows = sqlx::query!(
        "SELECT immutable FROM webhook_deliveries WHERE emitter_name = 'legacy'"
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!rows.is_empty());
    for r in rows {
        assert!(r.immutable, "backfilled row must be immutable");
    }
}

/// A receipt in state `stored` must map to delivery state `failed`.
#[sqlx::test]
async fn migration_stored_receipt_maps_to_failed_immutable(pool: PgPool) {
    setup_pre_migration(&pool).await;
    let id = seed_receipt(&pool, "stored").await;
    apply_migration_0002(&pool).await;

    let row = sqlx::query!(
        "SELECT state, immutable FROM webhook_deliveries WHERE receipt_id = $1",
        id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.state, "failed");
    assert!(row.immutable);
}

/// A receipt in state `dead_lettered` must map to delivery state `dead_lettered`.
#[sqlx::test]
async fn migration_dead_lettered_maps_correctly(pool: PgPool) {
    setup_pre_migration(&pool).await;
    let id = seed_receipt(&pool, "dead_lettered").await;
    apply_migration_0002(&pool).await;

    let row = sqlx::query!(
        "SELECT state FROM webhook_deliveries WHERE receipt_id = $1",
        id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.state, "dead_lettered");
}
