//! Round-trip integration test for the Kafka emitter.
//!
//! Spawns an ephemeral Kafka broker via testcontainers, publishes a
//! `NormalizedEvent` through `KafkaEmitter`, then consumes it back via a
//! `StreamConsumer` and asserts on full struct equality plus the
//! receipt-ID-as-record-key contract.

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(clippy::expect_used, reason = "expect is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::time::Duration;

use chrono::{TimeZone, Utc};
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_kafka::KafkaEmitter;
use rdkafka::ClientConfig;
use rdkafka::Message;
use rdkafka::consumer::{Consumer, StreamConsumer};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::kafka::apache::Kafka;
use uuid::Uuid;

const TOPIC: &str = "hookbox-test";

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
async fn kafka_emitter_round_trip() {
    let container = Kafka::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(9092).await.unwrap();
    let brokers = format!("{host}:{port}");

    let emitter = KafkaEmitter::new(
        &brokers,
        TOPIC.to_owned(),
        "hookbox-round-trip-test",
        "all",
        10_000,
    )
    .unwrap();

    // Build the consumer BEFORE publishing so the topic auto-creates and
    // we have a stable view of partition 0 from offset 0.
    let group_id = format!("hookbox-test-{}", Uuid::new_v4());
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("group.id", &group_id)
        .set("auto.offset.reset", "earliest")
        .set("enable.auto.commit", "false")
        .set("session.timeout.ms", "10000")
        .create()
        .unwrap();
    consumer.subscribe(&[TOPIC]).unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Wait up to 30 seconds for the message — first poll on a new consumer
    // group can be slow on cold Kafka.
    let message = tokio::time::timeout(Duration::from_secs(30), consumer.recv())
        .await
        .expect("timed out waiting for kafka message")
        .expect("consumer recv returned an error");

    assert_eq!(message.topic(), TOPIC, "topic mismatch");

    let key_bytes = message.key().expect("message should have a key");
    let key_str = std::str::from_utf8(key_bytes).unwrap();
    assert_eq!(
        key_str,
        event.receipt_id.to_string(),
        "kafka record key should equal receipt_id (partition routing)"
    );

    let payload_bytes = message.payload().expect("message should have a payload");
    let received: NormalizedEvent = serde_json::from_slice(payload_bytes).unwrap();
    assert_eq!(received, event);
}
