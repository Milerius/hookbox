//! Kafka emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a Kafka topic using the `rdkafka`
//! [`FutureProducer`].  Each event is serialised as JSON with the receipt
//! ID as the message key, ensuring deterministic partition assignment for
//! events originating from the same receipt.

use std::time::Duration;

use async_trait::async_trait;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A Kafka-backed [`Emitter`] that publishes events to a configured topic.
///
/// Messages are produced asynchronously via [`FutureProducer`].  The
/// [`NormalizedEvent::receipt_id`] is used as the Kafka message key so that
/// all messages for the same receipt land on the same partition.
pub struct KafkaEmitter {
    producer: FutureProducer,
    topic: String,
    timeout: Duration,
}

impl KafkaEmitter {
    /// Create a new [`KafkaEmitter`].
    ///
    /// # Arguments
    ///
    /// * `brokers` — comma-separated list of Kafka broker addresses.
    /// * `topic` — destination Kafka topic.
    /// * `client_id` — client identifier sent to the brokers.
    /// * `acks` — required acknowledgements (`"all"`, `"1"`, `"0"`).
    /// * `timeout_ms` — delivery timeout in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the producer cannot be created.
    pub fn new(
        brokers: &str,
        topic: String,
        client_id: &str,
        acks: &str,
        timeout_ms: u64,
    ) -> Result<Self, EmitError> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", brokers)
            .set("client.id", client_id)
            .set("acks", acks)
            .create()
            .map_err(|e| EmitError::Downstream(e.to_string()))?;

        Ok(Self {
            producer,
            topic,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

#[async_trait]
impl Emitter for KafkaEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload =
            serde_json::to_vec(event).map_err(|e| EmitError::Downstream(e.to_string()))?;
        let key = event.receipt_id.to_string();

        let record = FutureRecord::to(&self.topic)
            .key(&key)
            .payload(&payload);

        self.producer
            .send(record, self.timeout)
            .await
            .map_err(|(err, _)| EmitError::Downstream(err.to_string()))?;

        tracing::debug!(
            receipt_id = %event.receipt_id,
            topic = %self.topic,
            "event emitted to kafka"
        );

        Ok(())
    }
}
