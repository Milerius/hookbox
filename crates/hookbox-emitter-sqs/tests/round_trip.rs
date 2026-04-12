//! Round-trip integration tests for the SQS emitter.
//!
//! Spawns an ephemeral `LocalStack` container with SQS enabled, creates a
//! FIFO queue and a standard queue, publishes a `NormalizedEvent` through
//! `SqsEmitter::with_client` for each (no env mutation — static credentials
//! are passed directly through the SDK config), then receives the messages
//! back and asserts on full struct equality plus FIFO routing attributes
//! (`MessageGroupId`, `MessageDeduplicationId`).

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(clippy::expect_used, reason = "expect is acceptable in test code")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::collections::HashMap;

use aws_credential_types::Credentials;
use aws_sdk_sqs::Client as SqsClient;
use aws_sdk_sqs::config::{BehaviorVersion, Region};
use aws_sdk_sqs::types::{MessageSystemAttributeName, QueueAttributeName};
use chrono::{TimeZone, Utc};
use hookbox::state::ReceiptId;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;
use hookbox_emitter_sqs::SqsEmitter;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::localstack::LocalStack;

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

/// Build an `aws_sdk_sqs::Client` pointed at the `LocalStack` endpoint with
/// static credentials. No process-env mutation — `Credentials::new` constructs
/// the credential pair in-process and the SDK config builder accepts it
/// directly via `.credentials_provider(...)`.
fn build_sdk_client(endpoint_url: &str) -> SqsClient {
    let creds = Credentials::new(
        "test",          // access key id
        "test",          // secret access key
        None,            // session token
        None,            // expiry
        "hookbox-tests", // provider name
    );
    let config = aws_sdk_sqs::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .endpoint_url(endpoint_url)
        .credentials_provider(creds)
        .build();
    SqsClient::from_conf(config)
}

#[tokio::test]
async fn sqs_emitter_fifo_round_trip() {
    // LocalStack 3.0 (the testcontainers-modules default) does not return
    // `Attributes` via the JSON protocol. LocalStack 3.8+ fixes this.
    let container = LocalStack::default().with_tag("3.8").start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4566).await.unwrap();
    let endpoint_url = format!("http://{host}:{port}");

    // Build the SDK client once with static creds and reuse it for both queue
    // creation and the SqsEmitter under test.
    let sdk = build_sdk_client(&endpoint_url);

    let mut attrs: HashMap<QueueAttributeName, String> = HashMap::new();
    attrs.insert(QueueAttributeName::FifoQueue, "true".to_owned());
    attrs.insert(
        QueueAttributeName::ContentBasedDeduplication,
        "false".to_owned(),
    );
    let create_resp = sdk
        .create_queue()
        .queue_name("hookbox-test.fifo")
        .set_attributes(Some(attrs))
        .send()
        .await
        .unwrap();
    let queue_url = create_resp.queue_url.unwrap();

    let emitter = SqsEmitter::with_client(sdk.clone(), queue_url.clone(), true /* fifo */);

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    // Receive the message back, requesting all system attributes so we can
    // assert on the FIFO routing attributes set by the emitter.
    let recv_resp = sdk
        .receive_message()
        .queue_url(&queue_url)
        .max_number_of_messages(1)
        .wait_time_seconds(5)
        .message_system_attribute_names(MessageSystemAttributeName::All)
        .send()
        .await
        .unwrap();
    let messages = recv_resp.messages.unwrap_or_default();
    assert_eq!(messages.len(), 1, "expected exactly one message");
    let msg = &messages[0];

    let body = msg.body.as_deref().unwrap();
    let received: NormalizedEvent = serde_json::from_str(body).unwrap();
    assert_eq!(received, event);

    let attrs = msg.attributes.as_ref().unwrap();
    let group_id = attrs
        .get(&MessageSystemAttributeName::MessageGroupId)
        .unwrap();
    let dedup_id = attrs
        .get(&MessageSystemAttributeName::MessageDeduplicationId)
        .unwrap();
    assert_eq!(group_id, &event.provider_name);
    assert_eq!(dedup_id, &event.receipt_id.to_string());
}

#[tokio::test]
async fn sqs_emitter_standard_round_trip() {
    // LocalStack 3.0 (the testcontainers-modules default) does not return
    // `Attributes` via the JSON protocol. LocalStack 3.8+ fixes this.
    let container = LocalStack::default().with_tag("3.8").start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4566).await.unwrap();
    let endpoint_url = format!("http://{host}:{port}");

    let sdk = build_sdk_client(&endpoint_url);
    let create_resp = sdk
        .create_queue()
        .queue_name("hookbox-test")
        .send()
        .await
        .unwrap();
    let queue_url = create_resp.queue_url.unwrap();

    let emitter =
        SqsEmitter::with_client(sdk.clone(), queue_url.clone(), false /* standard */);

    let event = make_test_event();
    emitter.emit(&event).await.unwrap();

    let recv_resp = sdk
        .receive_message()
        .queue_url(&queue_url)
        .max_number_of_messages(1)
        .wait_time_seconds(5)
        .send()
        .await
        .unwrap();
    let messages = recv_resp.messages.unwrap_or_default();
    assert_eq!(messages.len(), 1, "expected exactly one message");

    let body = messages[0].body.as_deref().unwrap();
    let received: NormalizedEvent = serde_json::from_str(body).unwrap();
    assert_eq!(received, event);
}
