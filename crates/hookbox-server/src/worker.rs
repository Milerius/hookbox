//! Background retry worker for failed webhook emissions.

use std::time::Duration;

use hookbox::traits::{Emitter, Storage};
use hookbox::types::NormalizedEvent;
use hookbox_postgres::PostgresStorage;

/// Periodically retries receipts stuck in `EmitFailed` state.
pub struct RetryWorker {
    storage: PostgresStorage,
    emitter: Box<dyn Emitter + Send + Sync>,
    interval: Duration,
    max_attempts: i32,
}

impl RetryWorker {
    /// Create a new retry worker with the given storage, emitter, poll interval,
    /// and maximum retry attempts before dead-lettering.
    #[must_use]
    pub fn new(
        storage: PostgresStorage,
        emitter: Box<dyn Emitter + Send + Sync>,
        interval: Duration,
        max_attempts: i32,
    ) -> Self {
        Self {
            storage,
            emitter,
            interval,
            max_attempts,
        }
    }

    /// Spawn the worker as a background Tokio task.
    ///
    /// The returned [`JoinHandle`](tokio::task::JoinHandle) can be used to
    /// monitor or cancel the worker.
    #[must_use]
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    /// Main loop: sleep, query, retry, repeat.
    async fn run(&self) {
        loop {
            tokio::time::sleep(self.interval).await;
            match self.storage.query_for_retry(self.max_attempts).await {
                Ok(receipts) => {
                    if !receipts.is_empty() {
                        tracing::info!(count = receipts.len(), "retrying failed receipts");
                    }
                    for receipt in receipts {
                        self.retry_one(&receipt).await;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "retry worker: query failed");
                }
            }
        }
    }

    /// Attempt to re-emit a single failed receipt.
    async fn retry_one(&self, receipt: &hookbox::types::WebhookReceipt) {
        let event = NormalizedEvent {
            receipt_id: receipt.receipt_id,
            provider_name: receipt.provider_name.clone(),
            event_type: receipt.normalized_event_type.clone(),
            external_reference: receipt.external_reference.clone(),
            parsed_payload: receipt.parsed_payload.clone(),
            payload_hash: receipt.payload_hash.clone(),
            received_at: receipt.received_at,
            metadata: receipt.metadata.clone(),
        };

        match self.emitter.emit(&event).await {
            Ok(()) => {
                tracing::info!(receipt_id = %receipt.receipt_id, "retry: emit succeeded");
                let _ = self
                    .storage
                    .update_state(
                        receipt.receipt_id.0,
                        hookbox::ProcessingState::Emitted,
                        None,
                    )
                    .await;
            }
            Err(e) => {
                tracing::warn!(
                    receipt_id = %receipt.receipt_id,
                    error = %e,
                    "retry: emit failed"
                );
                let _ = self
                    .storage
                    .retry_failed(receipt.receipt_id.0, self.max_attempts)
                    .await;
            }
        }
    }
}
