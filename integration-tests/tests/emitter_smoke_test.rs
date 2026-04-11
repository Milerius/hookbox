//! Smoke tests for emitter adapters.
//!
//! These tests require real broker infrastructure (Docker) and are
//! marked `#[ignore]` by default.  Run them explicitly:
//!
//! ```bash
//! cargo test -p hookbox-integration-tests --test emitter_smoke_test -- --ignored
//! ```

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(clippy::expect_used, reason = "expect is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use chrono::Utc;
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

fn test_event() -> NormalizedEvent {
    NormalizedEvent {
        receipt_id: ReceiptId::new(),
        provider_name: "smoke-test".to_owned(),
        event_type: Some("test.smoke".to_owned()),
        external_reference: None,
        parsed_payload: Some(serde_json::json!({"smoke": true})),
        payload_hash: "deadbeef".to_owned(),
        received_at: Utc::now(),
        metadata: serde_json::json!({}),
    }
}

#[tokio::test]
#[ignore = "requires Kafka broker (docker-compose)"]
async fn kafka_emitter_smoke() {
    let emitter = hookbox_emitter_kafka::KafkaEmitter::new(
        "localhost:9092",
        "hookbox-smoke-test".to_owned(),
        "hookbox-smoke",
        "all",
        5000,
    )
    .expect("kafka emitter should be created");

    let event = test_event();
    emitter
        .emit(&event)
        .await
        .expect("kafka emit should succeed");
}

#[tokio::test]
#[ignore = "requires NATS server (docker-compose)"]
async fn nats_emitter_smoke() {
    let emitter = hookbox_emitter_nats::NatsEmitter::new(
        "nats://localhost:4222",
        "hookbox.smoke.test".to_owned(),
    )
    .await
    .expect("nats emitter should be created");

    let event = test_event();
    emitter
        .emit(&event)
        .await
        .expect("nats emit should succeed");
}

#[tokio::test]
#[ignore = "requires LocalStack SQS (docker-compose)"]
async fn sqs_emitter_smoke() {
    let queue_url = std::env::var("SQS_QUEUE_URL")
        .unwrap_or_else(|_| "http://localhost:4566/000000000000/hookbox-smoke-test".to_owned());
    let region = std::env::var("AWS_REGION").ok();
    let emitter = hookbox_emitter_sqs::SqsEmitter::new(
        queue_url,
        region.as_deref(),
        false,
    )
    .await
    .expect("sqs emitter should be created");

    let event = test_event();
    emitter.emit(&event).await.expect("sqs emit should succeed");
}
