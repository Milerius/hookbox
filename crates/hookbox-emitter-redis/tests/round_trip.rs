//! Round-trip integration test for the Redis Streams emitter.
//!
//! Spawns an ephemeral Redis container via testcontainers, publishes a
//! `NormalizedEvent` through `RedisEmitter`, then reads it back via `XREAD`
//! and asserts full struct equality.

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(clippy::expect_used, reason = "expect is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use chrono::{TimeZone, Utc};
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_redis::RedisEmitter;
use redis::AsyncCommands;
use redis::streams::{StreamReadOptions, StreamReadReply};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

const STREAM: &str = "hookbox.test";
const MAXLEN_STREAM: &str = "hookbox.test.maxlen";

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
async fn redis_emitter_round_trip() {
    let container = Redis::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://{host}:{port}");

    let emitter = RedisEmitter::new(&url, STREAM.to_owned(), None, 5000)
        .await
        .unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Consume back via XREAD and assert FULL struct equality.
    let client = redis::Client::open(url.as_str()).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let opts = StreamReadOptions::default().count(1);
    let reply: StreamReadReply = conn.xread_options(&[STREAM], &["0"], &opts).await.unwrap();

    assert_eq!(reply.keys.len(), 1, "expected exactly one stream in reply");
    let stream = &reply.keys[0];
    assert_eq!(stream.key, STREAM, "stream key mismatch");
    assert_eq!(stream.ids.len(), 1, "expected exactly one stream entry");

    let entry = &stream.ids[0];
    let data_value = entry
        .map
        .get("data")
        .expect("entry should have a `data` field");
    let data: String = redis::FromRedisValue::from_redis_value(data_value).unwrap();
    let received: NormalizedEvent = serde_json::from_str(&data).unwrap();

    assert_eq!(received, event);
}

#[tokio::test]
async fn redis_emitter_round_trip_with_maxlen() {
    // Exercises the `XADD ... MAXLEN ~` branch of `RedisEmitter::emit`,
    // which the default round-trip test (above, `maxlen = None`) skips.
    let container = Redis::default().start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://{host}:{port}");

    let emitter = RedisEmitter::new(&url, MAXLEN_STREAM.to_owned(), Some(100), 5000)
        .await
        .unwrap();

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    let client = redis::Client::open(url.as_str()).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let opts = StreamReadOptions::default().count(1);
    let reply: StreamReadReply = conn
        .xread_options(&[MAXLEN_STREAM], &["0"], &opts)
        .await
        .unwrap();

    assert_eq!(reply.keys.len(), 1, "expected exactly one stream in reply");
    let stream = &reply.keys[0];
    assert_eq!(stream.ids.len(), 1, "expected exactly one stream entry");

    let entry = &stream.ids[0];
    let data_value = entry
        .map
        .get("data")
        .expect("entry should have a `data` field");
    let data: String = redis::FromRedisValue::from_redis_value(data_value).unwrap();
    let received: NormalizedEvent = serde_json::from_str(&data).unwrap();

    assert_eq!(received, event);
}
