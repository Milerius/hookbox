//! NATS emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a NATS subject.  Each event is
//! serialised as JSON and published via the [`async_nats::Client`].

use async_trait::async_trait;
use bytes::Bytes;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A NATS-backed [`Emitter`] that publishes events to a configured subject.
pub struct NatsEmitter {
    client: async_nats::Client,
    subject: String,
}

impl NatsEmitter {
    /// Connect to a NATS server and return a new [`NatsEmitter`].
    ///
    /// # Arguments
    ///
    /// * `url` — NATS server URL (e.g. `"nats://127.0.0.1:4222"`).
    /// * `subject` — NATS subject to publish events to.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the connection cannot be
    /// established.
    pub async fn new(url: &str, subject: String) -> Result<Self, EmitError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))?;

        Ok(Self { client, subject })
    }
}

#[async_trait]
impl Emitter for NatsEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload =
            serde_json::to_vec(event).map_err(|e| EmitError::Downstream(e.to_string()))?;

        self.client
            .publish(self.subject.clone(), Bytes::from(payload))
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))?;

        tracing::debug!(
            receipt_id = %event.receipt_id,
            subject = %self.subject,
            "event emitted to nats"
        );

        Ok(())
    }
}
