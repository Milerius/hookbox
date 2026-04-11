//! Pipeline orchestration for the hookbox ingest flow.
//!
//! [`HookboxPipeline`] wires together the four extension-point traits
//! ([`Storage`], [`DedupeStrategy`], [`Emitter`], [`SignatureVerifier`]) into
//! the five-stage ingest pipeline:
//!
//! ```text
//! Receive → Verify → Dedupe → Store → Emit
//! ```
//!
//! Construction is done via [`HookboxPipelineBuilder`], obtained from
//! [`HookboxPipeline::builder()`].

use std::collections::HashMap;
use std::time::Instant;

use bytes::Bytes;
use chrono::Utc;
use http::HeaderMap;
use serde_json::json;

use crate::error::IngestError;
use crate::hash::compute_payload_hash;
use crate::state::{DedupeDecision, IngestResult, ProcessingState, ReceiptId, VerificationStatus};
use crate::traits::{DedupeStrategy, Emitter, SignatureVerifier, Storage};
use crate::types::{NormalizedEvent, WebhookReceipt};

/// Core pipeline that orchestrates webhook ingestion through five stages:
/// receive, verify, dedupe, store, and emit.
///
/// Generic over `S` (storage), `D` (deduplication), and `E` (emission) to
/// allow arbitrary backend implementations.
pub struct HookboxPipeline<S, D, E> {
    storage: S,
    dedupe: D,
    emitter: E,
    verifiers: HashMap<String, Box<dyn SignatureVerifier>>,
}

impl<S, D, E> HookboxPipeline<S, D, E>
where
    S: Storage,
    D: DedupeStrategy,
    E: Emitter,
{
    /// Create a new [`HookboxPipelineBuilder`].
    #[must_use]
    pub fn builder() -> HookboxPipelineBuilder<S, D, E> {
        HookboxPipelineBuilder {
            storage: None,
            dedupe: None,
            emitter: None,
            verifiers: HashMap::new(),
        }
    }

    /// Access the storage backend (e.g. for admin API queries).
    #[must_use]
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Access the emitter (e.g. for replay).
    #[must_use]
    pub fn emitter(&self) -> &E {
        &self.emitter
    }

    /// Ingest a single webhook event through the five-stage pipeline.
    ///
    /// Returns [`IngestResult::Accepted`] only after durable storage succeeds.
    /// Emit failures are recorded but do **not** prevent acceptance.
    ///
    /// # Errors
    ///
    /// Returns [`IngestError`] if the storage, dedupe, or verification layer
    /// encounters an unexpected infrastructure failure.
    #[expect(
        clippy::too_many_lines,
        reason = "pipeline stages are sequential and read best as one flow"
    )]
    #[tracing::instrument(skip(self, headers, body), fields(provider = %provider))]
    pub async fn ingest(
        &self,
        provider: &str,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<IngestResult, IngestError> {
        // ── Stage 1: Receive ──────────────────────────────────────────
        metrics::counter!("hookbox_webhooks_received_total", "provider" => provider.to_owned())
            .increment(1);
        let ingest_start = Instant::now();

        let receipt_id = ReceiptId::new();
        let payload_hash = compute_payload_hash(&body);
        let dedupe_key = format!("{provider}:{payload_hash}");

        // ── Stage 2: Verify ───────────────────────────────────────────
        let (verification_status, verification_reason) = if let Some(verifier) =
            self.verifiers.get(provider)
        {
            let result = verifier.verify(&headers, &body).await;
            match result.status {
                VerificationStatus::Failed => {
                    let reason = result
                        .reason
                        .unwrap_or_else(|| "verification_failed".to_owned());
                    tracing::warn!(
                        %receipt_id, %provider, reason = %reason,
                        "webhook verification failed"
                    );
                    metrics::counter!(
                        "hookbox_verification_results_total",
                        "provider" => provider.to_owned(),
                        "status" => "failed",
                        "reason" => reason.clone()
                    )
                    .increment(1);
                    metrics::counter!(
                        "hookbox_ingest_results_total",
                        "provider" => provider.to_owned(),
                        "result" => "verification_failed"
                    )
                    .increment(1);
                    metrics::histogram!("hookbox_ingest_duration_seconds")
                        .record(ingest_start.elapsed().as_secs_f64());
                    return Ok(IngestResult::VerificationFailed { reason });
                }
                VerificationStatus::Verified => {
                    let reason_label = result.reason.clone().unwrap_or_else(|| "none".to_owned());
                    metrics::counter!(
                        "hookbox_verification_results_total",
                        "provider" => provider.to_owned(),
                        "status" => "verified",
                        "reason" => reason_label
                    )
                    .increment(1);
                    (VerificationStatus::Verified, result.reason)
                }
                VerificationStatus::Skipped => {
                    let reason_label = result.reason.clone().unwrap_or_else(|| "none".to_owned());
                    metrics::counter!(
                        "hookbox_verification_results_total",
                        "provider" => provider.to_owned(),
                        "status" => "skipped",
                        "reason" => reason_label
                    )
                    .increment(1);
                    (VerificationStatus::Skipped, result.reason)
                }
            }
        } else {
            metrics::counter!(
                "hookbox_verification_results_total",
                "provider" => provider.to_owned(),
                "status" => "skipped",
                "reason" => "no_verifier_configured"
            )
            .increment(1);
            (
                VerificationStatus::Skipped,
                Some("no_verifier_configured".to_owned()),
            )
        };

        // ── Stage 3: Advisory dedupe ──────────────────────────────────
        let advisory = match self.dedupe.check(&dedupe_key, &payload_hash).await {
            Ok(decision) => decision,
            Err(e) => {
                metrics::counter!(
                    "hookbox_ingest_results_total",
                    "provider" => provider.to_owned(),
                    "result" => "dedupe_failed"
                )
                .increment(1);
                metrics::histogram!("hookbox_ingest_duration_seconds")
                    .record(ingest_start.elapsed().as_secs_f64());
                return Err(e.into());
            }
        };
        let dedupe_label = match advisory {
            DedupeDecision::New => "new",
            DedupeDecision::Duplicate => "duplicate",
            DedupeDecision::Conflict => "conflict",
        };
        metrics::counter!(
            "hookbox_dedupe_checks_total",
            "provider" => provider.to_owned(),
            "result" => dedupe_label
        )
        .increment(1);
        if advisory == DedupeDecision::Duplicate {
            tracing::info!(
                %receipt_id, %provider, %dedupe_key,
                "advisory dedupe flagged duplicate, proceeding to authoritative check"
            );
        }

        // ── Stage 4: Store (build receipt + durable write) ───────────
        let parsed_payload: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
        let raw_headers = headers_to_json(&headers);
        let now = Utc::now();

        let receipt = WebhookReceipt {
            receipt_id,
            provider_name: provider.to_owned(),
            provider_event_id: None,
            external_reference: None,
            dedupe_key: dedupe_key.clone(),
            payload_hash: payload_hash.clone(),
            raw_body: body.to_vec(),
            parsed_payload: parsed_payload.clone(),
            raw_headers,
            normalized_event_type: None,
            verification_status,
            verification_reason,
            processing_state: ProcessingState::Stored,
            emit_count: 0,
            last_error: None,
            received_at: now,
            processed_at: None,
            metadata: json!({}),
        };

        // ── (continued) Store durably ────────────────────────────────
        let store_start = Instant::now();
        let store_result = match self.storage.store(&receipt).await {
            Ok(r) => r,
            Err(e) => {
                metrics::counter!(
                    "hookbox_ingest_results_total",
                    "provider" => provider.to_owned(),
                    "result" => "store_failed"
                )
                .increment(1);
                metrics::histogram!("hookbox_ingest_duration_seconds")
                    .record(ingest_start.elapsed().as_secs_f64());
                return Err(e.into());
            }
        };
        metrics::histogram!("hookbox_store_duration_seconds")
            .record(store_start.elapsed().as_secs_f64());
        match store_result {
            crate::state::StoreResult::Duplicate { existing_id } => {
                tracing::info!(
                    %receipt_id, %existing_id, %provider,
                    "duplicate receipt detected by storage"
                );
                metrics::counter!(
                    "hookbox_ingest_results_total",
                    "provider" => provider.to_owned(),
                    "result" => "duplicate"
                )
                .increment(1);
                metrics::histogram!("hookbox_ingest_duration_seconds")
                    .record(ingest_start.elapsed().as_secs_f64());
                return Ok(IngestResult::Duplicate { existing_id });
            }
            crate::state::StoreResult::Stored => {
                if let Err(e) = self.dedupe.record(&dedupe_key, &payload_hash).await {
                    tracing::warn!(
                        %receipt_id, error = %e,
                        "failed to record in advisory dedupe cache"
                    );
                }
            }
        }

        // ── Stage 5: Emit ─────────────────────────────────────────────
        let event = NormalizedEvent {
            receipt_id,
            provider_name: provider.to_owned(),
            event_type: None,
            external_reference: None,
            parsed_payload,
            payload_hash,
            received_at: now,
            metadata: json!({}),
        };

        let emit_start = Instant::now();
        let emit_result = self.emitter.emit(&event).await;
        metrics::histogram!("hookbox_emit_duration_seconds")
            .record(emit_start.elapsed().as_secs_f64());

        let emit_label = if emit_result.is_ok() {
            "success"
        } else {
            "failure"
        };
        metrics::counter!(
            "hookbox_emit_results_total",
            "provider" => provider.to_owned(),
            "result" => emit_label
        )
        .increment(1);

        if let Err(e) = emit_result {
            tracing::warn!(%receipt_id, error = %e, "emit failed, receipt remains accepted");
            let _ = self
                .storage
                .update_state(
                    receipt_id.0,
                    ProcessingState::EmitFailed,
                    Some(&e.to_string()),
                )
                .await;
        } else {
            let _ = self
                .storage
                .update_state(receipt_id.0, ProcessingState::Emitted, None)
                .await;
        }

        tracing::info!(%receipt_id, %provider, "webhook accepted");
        metrics::counter!(
            "hookbox_ingest_results_total",
            "provider" => provider.to_owned(),
            "result" => "accepted"
        )
        .increment(1);
        metrics::histogram!("hookbox_ingest_duration_seconds")
            .record(ingest_start.elapsed().as_secs_f64());
        Ok(IngestResult::Accepted { receipt_id })
    }
}

/// Builder for [`HookboxPipeline`].
///
/// Obtained via [`HookboxPipeline::builder()`].
///
/// # Panics
///
/// [`build()`](HookboxPipelineBuilder::build) panics if `storage`, `dedupe`,
/// or `emitter` have not been set.
pub struct HookboxPipelineBuilder<S, D, E> {
    storage: Option<S>,
    dedupe: Option<D>,
    emitter: Option<E>,
    verifiers: HashMap<String, Box<dyn SignatureVerifier>>,
}

impl<S, D, E> HookboxPipelineBuilder<S, D, E>
where
    S: Storage,
    D: DedupeStrategy,
    E: Emitter,
{
    /// Set the storage backend.
    #[must_use]
    pub fn storage(mut self, s: S) -> Self {
        self.storage = Some(s);
        self
    }

    /// Set the deduplication strategy.
    #[must_use]
    pub fn dedupe(mut self, d: D) -> Self {
        self.dedupe = Some(d);
        self
    }

    /// Set the emitter.
    #[must_use]
    pub fn emitter(mut self, e: E) -> Self {
        self.emitter = Some(e);
        self
    }

    /// Register a signature verifier.
    ///
    /// The verifier is keyed by its [`SignatureVerifier::provider_name()`].
    /// Multiple verifiers for different providers can be registered.
    #[must_use]
    pub fn verifier(mut self, v: impl SignatureVerifier + 'static) -> Self {
        self.verifiers
            .insert(v.provider_name().to_owned(), Box::new(v));
        self
    }

    /// Build the pipeline.
    ///
    /// # Panics
    ///
    /// Panics if `storage`, `dedupe`, or `emitter` have not been set.
    #[expect(
        clippy::expect_used,
        reason = "builder panics by design when required fields are missing"
    )]
    #[must_use]
    pub fn build(self) -> HookboxPipeline<S, D, E> {
        HookboxPipeline {
            storage: self.storage.expect("storage must be set before build()"),
            dedupe: self.dedupe.expect("dedupe must be set before build()"),
            emitter: self.emitter.expect("emitter must be set before build()"),
            verifiers: self.verifiers,
        }
    }
}

/// Convert an HTTP [`HeaderMap`] to a JSON object.
///
/// Header names become keys and header values become string values.
/// Multiple values for the same header are concatenated with `, `.
fn headers_to_json(headers: &HeaderMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for key in headers.keys() {
        let values: Vec<&str> = headers
            .get_all(key)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        map.insert(
            key.as_str().to_owned(),
            serde_json::Value::String(values.join(", ")),
        );
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#[expect(clippy::panic, reason = "panic is acceptable in test assertions")]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use bytes::Bytes;
    use http::HeaderMap;
    use uuid::Uuid;

    use super::*;
    use crate::dedupe::InMemoryRecentDedupe;
    use crate::emitter::CallbackEmitter;
    use crate::error::{EmitError, StorageError};
    use crate::state::{StoreResult, VerificationResult, VerificationStatus};
    use crate::traits::{SignatureVerifier, Storage};
    use crate::types::{NormalizedEvent, ReceiptFilter, WebhookReceipt};

    // ── Test helpers ──────────────────────────────────────────────────

    /// In-memory storage implementation for tests.
    struct MemoryStorage {
        receipts: Mutex<Vec<WebhookReceipt>>,
    }

    impl MemoryStorage {
        fn new() -> Self {
            Self {
                receipts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Storage for MemoryStorage {
        async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
            let mut receipts = self
                .receipts
                .lock()
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            if let Some(existing) = receipts.iter().find(|r| r.dedupe_key == receipt.dedupe_key) {
                return Ok(StoreResult::Duplicate {
                    existing_id: existing.receipt_id,
                });
            }
            receipts.push(receipt.clone());
            Ok(StoreResult::Stored)
        }

        async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
            let receipts = self
                .receipts
                .lock()
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(receipts.iter().find(|r| r.receipt_id.0 == id).cloned())
        }

        async fn update_state(
            &self,
            id: Uuid,
            state: ProcessingState,
            error: Option<&str>,
        ) -> Result<(), StorageError> {
            let mut receipts = self
                .receipts
                .lock()
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            if let Some(receipt) = receipts.iter_mut().find(|r| r.receipt_id.0 == id) {
                receipt.processing_state = state;
                receipt.last_error = error.map(String::from);
            }
            Ok(())
        }

        async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
            let receipts = self
                .receipts
                .lock()
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(receipts.clone())
        }
    }

    /// Verifier that always returns [`VerificationStatus::Verified`].
    struct PassVerifier;

    #[async_trait]
    impl SignatureVerifier for PassVerifier {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Verified,
                reason: Some("signature_valid".to_owned()),
            }
        }
    }

    /// Verifier that always returns [`VerificationStatus::Failed`].
    struct FailVerifier;

    #[async_trait]
    impl SignatureVerifier for FailVerifier {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_signature".to_owned()),
            }
        }
    }

    type NoopEmitter = CallbackEmitter<
        fn(NormalizedEvent) -> std::future::Ready<Result<(), EmitError>>,
        std::future::Ready<Result<(), EmitError>>,
    >;

    fn noop_emitter() -> NoopEmitter {
        CallbackEmitter::new((|_event: NormalizedEvent| std::future::ready(Ok(()))) as fn(_) -> _)
    }

    // ── Tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ingest_accepted_on_valid_webhook() {
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(noop_emitter())
            .verifier(PassVerifier)
            .build();

        let body = Bytes::from(r#"{"event":"payment.completed"}"#);
        let headers = HeaderMap::new();

        let result = pipeline.ingest("test-provider", headers, body).await;
        assert!(result.is_ok(), "ingest should succeed");

        match result.unwrap() {
            IngestResult::Accepted { receipt_id } => {
                assert_ne!(receipt_id.0, Uuid::nil(), "receipt_id should be non-nil");
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ingest_duplicate_on_same_body() {
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(noop_emitter())
            .verifier(PassVerifier)
            .build();

        let body = Bytes::from(r#"{"event":"payment.completed"}"#);

        let first = pipeline
            .ingest("test-provider", HeaderMap::new(), body.clone())
            .await;
        assert!(matches!(first, Ok(IngestResult::Accepted { .. })));

        let second = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;
        assert!(
            matches!(second, Ok(IngestResult::Duplicate { .. })),
            "second ingest of same body should be Duplicate, got {second:?}"
        );
    }

    #[tokio::test]
    async fn ingest_verification_failed() {
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(noop_emitter())
            .verifier(FailVerifier)
            .build();

        let body = Bytes::from(r#"{"event":"payment.completed"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        match result {
            Ok(IngestResult::VerificationFailed { reason }) => {
                assert_eq!(reason, "invalid_signature");
            }
            other => panic!("expected VerificationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ingest_skipped_verification_when_no_verifier() {
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(noop_emitter())
            // No verifier registered for "unknown-provider".
            .build();

        let body = Bytes::from(r#"{"event":"payment.completed"}"#);
        let result = pipeline
            .ingest("unknown-provider", HeaderMap::new(), body)
            .await;

        assert!(
            matches!(result, Ok(IngestResult::Accepted { .. })),
            "should be accepted with skipped verification, got {result:?}"
        );
    }

    /// Emitter that always returns a downstream error.
    struct FailEmitter;

    #[async_trait]
    impl Emitter for FailEmitter {
        async fn emit(&self, _event: &NormalizedEvent) -> Result<(), EmitError> {
            Err(EmitError::Downstream("test_failure".to_owned()))
        }
    }

    #[tokio::test]
    async fn ingest_accepted_even_when_emit_fails() {
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(FailEmitter)
            .verifier(PassVerifier)
            .build();

        let body = Bytes::from(r#"{"event":"payment.completed"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        assert!(
            matches!(result, Ok(IngestResult::Accepted { .. })),
            "pipeline should return Accepted even when the emitter fails, got {result:?}"
        );
    }
}
