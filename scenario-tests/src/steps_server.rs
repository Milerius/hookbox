//! Cucumber step definitions for server BDD scenarios.
//!
//! Exercises `hookbox-server` end-to-end via HTTP against the
//! [`crate::server_harness::TestServer`] (in-process Axum + testcontainer
//! Postgres).  Covers fan-out durability, retry, DLQ, and replay flows.

#![expect(clippy::panic, reason = "panic is acceptable in BDD test assertions")]
#![expect(clippy::unused_async, reason = "cucumber step functions must be async")]
#![expect(clippy::expect_used, reason = "expect is acceptable in BDD test setup")]
#![expect(
    clippy::missing_panics_doc,
    reason = "BDD step functions panic on failure by design"
)]

use std::time::Duration;

use cucumber::{given, then, when};
use sqlx::Row as _;
use tokio::time::sleep;

use crate::fake_emitter::{EmitterBehavior, FakeEmitter};
use crate::server_harness::{EmitterSpec, FAKE_PROVIDER, TestServer};
use crate::world::ServerWorld;

/// Maximum wallclock time a step will wait for an async convergence check.
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(20);
/// Polling interval while waiting for convergence.
const CONVERGE_POLL: Duration = Duration::from_millis(100);

async fn converge<F, Fut>(mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + CONVERGE_TIMEOUT;
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        sleep(CONVERGE_POLL).await;
    }
}

// ── Given steps ──────────────────────────────────────────────────────────

/// Start a server with `primary` and `secondary` healthy emitters.
#[given(regex = r"^a running hookbox server with two healthy emitters$")]
pub async fn given_two_healthy(world: &mut ServerWorld) {
    let primary = std::sync::Arc::new(FakeEmitter::new("primary", EmitterBehavior::Healthy));
    let secondary = std::sync::Arc::new(FakeEmitter::new("secondary", EmitterBehavior::Healthy));
    let server = TestServer::start(vec![
        EmitterSpec::new("primary", primary),
        EmitterSpec::new("secondary", secondary),
    ])
    .await
    .expect("start test server");
    world.install(server);
}

/// Start a server where `name` fails until the second attempt, then recovers.
#[given(regex = r#"^a running hookbox server where emitter "([^"]+)" fails once then recovers$"#)]
pub async fn given_flaky(world: &mut ServerWorld, name: String) {
    let healthy = std::sync::Arc::new(FakeEmitter::new("healthy", EmitterBehavior::Healthy));
    let flaky = std::sync::Arc::new(FakeEmitter::new(
        name.clone(),
        EmitterBehavior::FailUntilAttempt(2),
    ));
    let server = TestServer::start(vec![
        EmitterSpec::new("healthy", healthy),
        EmitterSpec::new(name, flaky),
    ])
    .await
    .expect("start test server");
    world.install(server);
}

/// Start a server where `name` is a permanently-failing emitter.
#[given(regex = r#"^a running hookbox server where emitter "([^"]+)" always fails$"#)]
pub async fn given_always_fail(world: &mut ServerWorld, name: String) {
    let healthy = std::sync::Arc::new(FakeEmitter::new("healthy", EmitterBehavior::Healthy));
    let broken = std::sync::Arc::new(FakeEmitter::new(name.clone(), EmitterBehavior::AlwaysFail));
    let server = TestServer::start(vec![
        EmitterSpec::new("healthy", healthy),
        EmitterSpec::new(name, broken),
    ])
    .await
    .expect("start test server");
    world.install(server);
}

// ── When steps ───────────────────────────────────────────────────────────

/// POST a minimal JSON webhook carrying `id` as the dedupe key.
#[when(regex = r#"^I POST a webhook with id "([^"]+)"$"#)]
pub async fn when_post(world: &mut ServerWorld, id: String) {
    let base = world.base_url().to_owned();
    let body = serde_json::json!({ "id": id });
    let client = world.client();
    let resp = client
        .post(format!("{base}/webhooks/{FAKE_PROVIDER}"))
        .json(&body)
        .send()
        .await
        .expect("post webhook");
    world.last_status = Some(resp.status().as_u16());
    let text = resp.text().await.unwrap_or_default();
    world.last_body = Some(text);
}

// ── Then steps ───────────────────────────────────────────────────────────

/// Assert the most recent HTTP response matches `code`.
#[then(regex = r"^the response status is (\d+)$")]
pub async fn then_status(world: &mut ServerWorld, code: u16) {
    assert_eq!(world.last_status, Some(code), "HTTP status mismatch");
}

/// Wait up to `CONVERGE_TIMEOUT` for `name` to receive at least `count` events.
#[then(regex = r#"^emitter "([^"]+)" eventually receives (\d+) event(?:s)?$"#)]
pub async fn then_emitter_receives(world: &mut ServerWorld, name: String, count: usize) {
    let fake = world
        .server()
        .emitter(&name)
        .unwrap_or_else(|| panic!("no emitter named {name}"));
    let ok = converge(|| {
        let fake = fake.clone();
        async move { fake.received_count() >= count }
    })
    .await;
    assert!(
        ok,
        "emitter {name} expected {count} events, got {}",
        fake.received_count()
    );
}

/// Poll `webhook_deliveries` until `expected` rows exist for `emitter` in `state`.
#[then(regex = r#"^(\d+) delivery rows? exists? for emitter "([^"]+)" in state "([^"]+)"$"#)]
pub async fn then_delivery_rows(
    world: &mut ServerWorld,
    expected: i64,
    emitter: String,
    state: String,
) {
    let pool = world.server().pool.clone();
    let ok = converge(|| {
        let pool = pool.clone();
        let emitter = emitter.clone();
        let state = state.clone();
        async move {
            let row = sqlx::query(
                "SELECT COUNT(*)::bigint AS c FROM webhook_deliveries \
                 WHERE emitter_name = $1 AND state = $2",
            )
            .bind(&emitter)
            .bind(&state)
            .fetch_one(&pool)
            .await;
            match row {
                Ok(r) => r.try_get::<i64, _>("c").unwrap_or(-1) == expected,
                Err(_) => false,
            }
        }
    })
    .await;
    assert!(
        ok,
        "delivery rows converge failed: emitter={emitter} state={state} expected={expected}"
    );
}

/// Wait until `name` has made at least `min` total emit attempts.
#[then(regex = r#"^emitter "([^"]+)" has at least (\d+) attempt(?:s)?$"#)]
pub async fn then_attempts_at_least(world: &mut ServerWorld, name: String, min: u32) {
    let fake = world
        .server()
        .emitter(&name)
        .unwrap_or_else(|| panic!("no emitter named {name}"));
    let ok = converge(|| {
        let fake = fake.clone();
        async move { fake.attempt_count() >= min }
    })
    .await;
    assert!(
        ok,
        "emitter {name} attempts: expected ≥ {min}, got {}",
        fake.attempt_count()
    );
}
