//! Round-trip integration test for the NATS emitter.
//!
//! Spawns an ephemeral NATS server via testcontainers, subscribes to the
//! target subject, publishes a `NormalizedEvent` through `NatsEmitter`, then
//! reads the message back from the subscription and asserts on full struct
//! equality.

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(clippy::expect_used, reason = "expect is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::time::Duration;

use chrono::{TimeZone, Utc};
use futures::StreamExt;
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_nats::NatsEmitter;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::nats::Nats;

const SUBJECT: &str = "hookbox.test";

fn make_test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "round-trip".to_owned(),
        event_type: Some("test.round_trip".to_owned()),
        external_reference: Some("ext-ref-1".to_owned()),
        parsed_payload: Some(serde_json::json!({"k": "v"})),
        payload_hash: "deadbeefcafe".to_owned(),
        received_at: Utc.with_ymd_and_hms(2026, 4, 12, 10, 0, 0).unwrap(),
        metadata: serde_json::json!({"meta": 1}),
    }
}

#[tokio::test]
async fn nats_emitter_round_trip() {
    let container = Nats::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{host}:{port}");

    // Subscribe BEFORE publishing — NATS drops messages with no subscribers.
    let subscriber_client = async_nats::connect(&url).await.unwrap();
    let mut subscription = subscriber_client.subscribe(SUBJECT).await.unwrap();
    // `subscribe()` returns before the SUB protocol message round-trips to the
    // server. `flush()` blocks until the server has actually registered our
    // subscription, closing the publish-before-subscribe race window.
    subscriber_client.flush().await.unwrap();

    let emitter = NatsEmitter::new(&url, SUBJECT.to_owned()).await.unwrap();
    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Wait up to 5 seconds for the message to arrive.
    let message = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("timed out waiting for nats message")
        .expect("subscription returned None");

    assert_eq!(message.subject.as_str(), SUBJECT, "subject mismatch");

    let received: NormalizedEvent = serde_json::from_slice(&message.payload).unwrap();
    assert_eq!(received, event);
}
