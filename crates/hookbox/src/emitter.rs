//! Built-in [`Emitter`] implementations.
//!
//! This module provides two ready-to-use emitters:
//!
//! - [`CallbackEmitter`] — wraps an async closure, useful for tests and simple
//!   single-process consumers.
//! - [`ChannelEmitter`] — forwards events over a Tokio MPSC channel, useful
//!   when the consumer lives on a separate task.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::EmitError;
use crate::traits::Emitter;
use crate::types::NormalizedEvent;

/// An [`Emitter`] that delegates to an async closure.
///
/// Useful for tests and lightweight single-process integrations where a full
/// channel or queue is unnecessary overhead.
///
/// # Example
///
/// ```rust
/// use hookbox::emitter::CallbackEmitter;
/// use hookbox::types::NormalizedEvent;
///
/// # async fn example() {
/// let emitter = CallbackEmitter::new(|event: NormalizedEvent| async move {
///     // process event …
///     Ok(())
/// });
/// # }
/// ```
pub struct CallbackEmitter<F, Fut>
where
    F: Fn(NormalizedEvent) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), EmitError>> + Send,
{
    handler: Arc<F>,
}

impl<F, Fut> CallbackEmitter<F, Fut>
where
    F: Fn(NormalizedEvent) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), EmitError>> + Send,
{
    /// Create a new [`CallbackEmitter`] that calls `handler` for every event.
    pub fn new(handler: F) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }
}

#[async_trait]
impl<F, Fut> Emitter for CallbackEmitter<F, Fut>
where
    F: Fn(NormalizedEvent) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), EmitError>> + Send + 'static,
{
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        (self.handler)(event.clone()).await
    }
}

/// An [`Emitter`] that sends events over a Tokio MPSC channel.
///
/// The paired [`mpsc::Receiver`] is returned from [`ChannelEmitter::new`] and
/// should be held by the downstream consumer task.
///
/// Send errors (e.g. the receiver was dropped) are mapped to
/// [`EmitError::Downstream`].
pub struct ChannelEmitter {
    sender: mpsc::Sender<NormalizedEvent>,
}

impl ChannelEmitter {
    /// Create a new [`ChannelEmitter`] with the given channel buffer capacity.
    ///
    /// Returns the emitter and the receiving half of the channel.
    #[must_use]
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<NormalizedEvent>) {
        let (sender, receiver) = mpsc::channel(buffer);
        (Self { sender }, receiver)
    }
}

#[async_trait]
impl Emitter for ChannelEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        self.sender
            .send(event.clone())
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use chrono::Utc;

    use super::{CallbackEmitter, ChannelEmitter};
    use crate::traits::Emitter;
    use crate::types::{NormalizedEvent, ReceiptId};

    fn test_event() -> NormalizedEvent {
        NormalizedEvent {
            receipt_id: ReceiptId::new(),
            provider_name: "test".to_owned(),
            event_type: Some("payment.completed".to_owned()),
            external_reference: None,
            parsed_payload: Some(serde_json::json!({"status": "ok"})),
            payload_hash: "abc123".to_owned(),
            received_at: Utc::now(),
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn callback_emitter_calls_handler() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let emitter = CallbackEmitter::new(move |_event: NormalizedEvent| {
            let c = Arc::clone(&counter_clone);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        let event = test_event();
        let result = emitter.emit(&event).await;
        assert!(result.is_ok(), "emit should succeed");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "handler should have been called once"
        );
    }

    #[tokio::test]
    async fn channel_emitter_sends_event() {
        let (emitter, mut receiver) = ChannelEmitter::new(8);
        let event = test_event();

        let result = emitter.emit(&event).await;
        assert!(result.is_ok(), "emit should succeed");

        let received = receiver.try_recv();
        assert!(received.is_ok(), "receiver should have an event");

        if let Ok(received_event) = received {
            assert_eq!(received_event.provider_name, event.provider_name);
            assert_eq!(received_event.event_type, event.event_type);
            assert_eq!(received_event.payload_hash, event.payload_hash);
        }
    }
}
