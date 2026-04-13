//! Build the runtime [`Emitter`] from configuration.
//!
//! Extracts the kafka / nats / sqs / redis / channel selection match out of
//! the `serve` startup path so the validation branches can be unit-tested
//! without a running broker. The successful construction paths for the four
//! production emitters are exercised by the per-crate testcontainer
//! round-trip integration tests.

use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::mpsc;

use hookbox::NormalizedEvent;
use hookbox::emitter::ChannelEmitter;
use hookbox::traits::Emitter;

use crate::config::EmitterConfig;

/// Outcome of [`build_emitter`].
///
/// The two variants distinguish production emitters (which are owned and
/// used directly) from the development `channel` emitter, which produces a
/// paired receiver that the caller must drain.
pub enum BuiltEmitter {
    /// A ready-to-use production emitter (kafka / nats / sqs / redis).
    Ready(Arc<dyn Emitter + Send + Sync>),
    /// The in-process channel emitter, plus its receiver. The caller is
    /// responsible for spawning a drain task on the receiver.
    Channel {
        /// The channel-backed emitter, type-erased to match `Ready`.
        emitter: Arc<dyn Emitter + Send + Sync>,
        /// The paired receiver. Drain it (e.g. via `tokio::spawn`) so emits
        /// don't fail with a closed channel.
        rx: mpsc::Receiver<NormalizedEvent>,
    },
}

/// Build a [`BuiltEmitter`] from configuration.
///
/// # Errors
///
/// Returns an error if:
/// - the configured `type` is unknown,
/// - the matching `[emitter.<type>]` section is missing for a production
///   backend, or
/// - the underlying emitter constructor fails (e.g. unreachable broker).
pub async fn build_emitter(cfg: &EmitterConfig) -> anyhow::Result<BuiltEmitter> {
    match cfg.emitter_type.as_str() {
        "kafka" => {
            let kafka = cfg.kafka.as_ref().ok_or_else(|| {
                anyhow::anyhow!("[emitter.kafka] section required when type = \"kafka\"")
            })?;
            let emitter = hookbox_emitter_kafka::KafkaEmitter::new(
                &kafka.brokers,
                kafka.topic.clone(),
                &kafka.client_id,
                &kafka.acks,
                kafka.timeout_ms,
            )
            .context("failed to create Kafka emitter")?;
            tracing::info!(brokers = %kafka.brokers, topic = %kafka.topic, "emitter: kafka");
            Ok(BuiltEmitter::Ready(Arc::new(emitter)))
        }
        "nats" => {
            let nats = cfg.nats.as_ref().ok_or_else(|| {
                anyhow::anyhow!("[emitter.nats] section required when type = \"nats\"")
            })?;
            let emitter = hookbox_emitter_nats::NatsEmitter::new(&nats.url, nats.subject.clone())
                .await
                .context("failed to create NATS emitter")?;
            tracing::info!(subject = %nats.subject, "emitter: nats");
            Ok(BuiltEmitter::Ready(Arc::new(emitter)))
        }
        "sqs" => {
            let sqs = cfg.sqs.as_ref().ok_or_else(|| {
                anyhow::anyhow!("[emitter.sqs] section required when type = \"sqs\"")
            })?;
            let emitter = hookbox_emitter_sqs::SqsEmitter::new(
                sqs.queue_url.clone(),
                sqs.region.as_deref(),
                sqs.fifo,
                sqs.endpoint_url.as_deref(),
            )
            .await
            .context("failed to create SQS emitter")?;
            tracing::info!(queue_url = %sqs.queue_url, fifo = %sqs.fifo, "emitter: sqs");
            Ok(BuiltEmitter::Ready(Arc::new(emitter)))
        }
        "redis" => {
            let redis = cfg.redis.as_ref().ok_or_else(|| {
                anyhow::anyhow!("[emitter.redis] section required when type = \"redis\"")
            })?;
            let emitter = hookbox_emitter_redis::RedisEmitter::new(
                &redis.url,
                redis.stream.clone(),
                redis.maxlen,
                redis.timeout_ms,
            )
            .await
            .context("failed to create Redis emitter")?;
            tracing::info!(stream = %redis.stream, "emitter: redis");
            Ok(BuiltEmitter::Ready(Arc::new(emitter)))
        }
        "channel" => {
            let (channel_emitter, rx) = ChannelEmitter::new(1024);
            tracing::info!("emitter: channel (development drain)");
            Ok(BuiltEmitter::Channel {
                emitter: Arc::new(channel_emitter),
                rx,
            })
        }
        other => anyhow::bail!(
            "unknown emitter type {other:?}; valid values: kafka, nats, sqs, redis, channel"
        ),
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
#[expect(clippy::panic, reason = "panic is acceptable in test assertions")]
mod tests {
    use super::*;

    fn cfg_with_type(t: &str) -> EmitterConfig {
        EmitterConfig {
            emitter_type: t.to_owned(),
            ..EmitterConfig::default()
        }
    }

    /// Run `build_emitter` and return the error message, panicking if the
    /// build unexpectedly succeeds. `BuiltEmitter` does not implement `Debug`
    /// (it holds an `Arc<dyn Emitter>`), so the standard `expect_err` does
    /// not apply.
    async fn expect_build_error(cfg: &EmitterConfig) -> String {
        match build_emitter(cfg).await {
            Ok(_built) => panic!("expected build_emitter to return Err"),
            Err(e) => e.to_string(),
        }
    }

    #[tokio::test]
    async fn unknown_type_returns_error() {
        let cfg = cfg_with_type("bogus");
        let msg = expect_build_error(&cfg).await;
        assert!(msg.contains("unknown emitter type"), "msg = {msg}");
        assert!(msg.contains("bogus"), "msg = {msg}");
    }

    #[tokio::test]
    async fn kafka_missing_section_returns_error() {
        let cfg = cfg_with_type("kafka");
        let msg = expect_build_error(&cfg).await;
        assert!(msg.contains("[emitter.kafka]"), "msg = {msg}");
    }

    #[tokio::test]
    async fn nats_missing_section_returns_error() {
        let cfg = cfg_with_type("nats");
        let msg = expect_build_error(&cfg).await;
        assert!(msg.contains("[emitter.nats]"), "msg = {msg}");
    }

    #[tokio::test]
    async fn sqs_missing_section_returns_error() {
        let cfg = cfg_with_type("sqs");
        let msg = expect_build_error(&cfg).await;
        assert!(msg.contains("[emitter.sqs]"), "msg = {msg}");
    }

    #[tokio::test]
    async fn redis_missing_section_returns_error() {
        let cfg = cfg_with_type("redis");
        let msg = expect_build_error(&cfg).await;
        assert!(msg.contains("[emitter.redis]"), "msg = {msg}");
    }

    #[tokio::test]
    async fn kafka_with_section_builds_ready_variant() {
        // `KafkaEmitter::new` is synchronous and uses rdkafka's ClientConfig,
        // which does not actually connect — it only validates the property
        // map. So a well-formed config returns `Ok` without a broker, and we
        // exercise the kafka arm's success path (including the tracing and
        // `BuiltEmitter::Ready` construction).
        let cfg = EmitterConfig {
            emitter_type: "kafka".to_owned(),
            kafka: Some(crate::config::KafkaEmitterConfig {
                brokers: "localhost:9092".to_owned(),
                topic: "coverage-test".to_owned(),
                client_id: "hookbox-test".to_owned(),
                acks: "all".to_owned(),
                timeout_ms: 1000,
            }),
            ..EmitterConfig::default()
        };
        let built = build_emitter(&cfg)
            .await
            .expect("kafka build should succeed");
        match built {
            BuiltEmitter::Ready(_) => {}
            BuiltEmitter::Channel { .. } => panic!("expected Ready variant, got Channel"),
        }
    }

    #[tokio::test]
    async fn nats_with_section_fails_on_unreachable_broker() {
        // `async_nats::connect` eagerly dials the broker, so pointing at a
        // closed port reliably trips the `.context("failed to create NATS
        // emitter")` branch and covers the nats arm end-to-end.
        let cfg = EmitterConfig {
            emitter_type: "nats".to_owned(),
            nats: Some(crate::config::NatsEmitterConfig {
                url: "nats://127.0.0.1:1".to_owned(),
                subject: "coverage-test".to_owned(),
            }),
            ..EmitterConfig::default()
        };
        let msg = expect_build_error(&cfg).await;
        assert!(msg.contains("failed to create NATS emitter"), "msg = {msg}");
    }

    #[tokio::test]
    async fn sqs_with_section_builds_ready_variant() {
        // The AWS SQS client is lazy: `aws_config::from_env()` +
        // `SqsClient::new` does not make network calls during construction,
        // so we exercise the sqs arm's success path without reaching the
        // `.context("failed to create SQS emitter")` branch.
        let cfg = EmitterConfig {
            emitter_type: "sqs".to_owned(),
            sqs: Some(crate::config::SqsEmitterConfig {
                queue_url: "http://localhost:4566/000000000000/coverage-test".to_owned(),
                region: Some("us-east-1".to_owned()),
                fifo: false,
                endpoint_url: Some("http://localhost:4566".to_owned()),
            }),
            ..EmitterConfig::default()
        };
        let built = build_emitter(&cfg).await.expect("sqs build should succeed");
        match built {
            BuiltEmitter::Ready(_) => {}
            BuiltEmitter::Channel { .. } => panic!("expected Ready variant, got Channel"),
        }
    }

    #[tokio::test]
    async fn redis_with_section_fails_on_unreachable_server() {
        // `ConnectionManager::new_with_config` eagerly dials the redis server
        // with the configured timeout. Pointing at a closed port + a short
        // timeout trips the `.context("failed to create Redis emitter")`
        // branch quickly.
        let cfg = EmitterConfig {
            emitter_type: "redis".to_owned(),
            redis: Some(crate::config::RedisEmitterConfig {
                url: "redis://127.0.0.1:1".to_owned(),
                stream: "coverage-test".to_owned(),
                maxlen: None,
                timeout_ms: 200,
            }),
            ..EmitterConfig::default()
        };
        let msg = expect_build_error(&cfg).await;
        assert!(
            msg.contains("failed to create Redis emitter"),
            "msg = {msg}"
        );
    }

    #[tokio::test]
    async fn channel_returns_channel_variant() {
        let cfg = cfg_with_type("channel");
        let built = build_emitter(&cfg).await.expect("build should succeed");
        match built {
            BuiltEmitter::Channel { emitter, mut rx } => {
                // Confirm the returned emitter and receiver are paired by
                // emitting one event and observing it on the receiver.
                let event = NormalizedEvent {
                    receipt_id: hookbox::state::ReceiptId::new(),
                    provider_name: "test".to_owned(),
                    event_type: None,
                    external_reference: None,
                    parsed_payload: None,
                    payload_hash: "h".to_owned(),
                    received_at: chrono::Utc::now(),
                    metadata: serde_json::json!({}),
                };
                emitter.emit(&event).await.expect("emit should succeed");
                let received = rx.try_recv().expect("event should arrive on rx");
                assert_eq!(received.provider_name, "test");
            }
            BuiltEmitter::Ready(_) => panic!("expected Channel variant, got Ready"),
        }
    }
}
