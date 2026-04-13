//! Pipeline orchestration for the hookbox ingest flow.
//!
//! [`HookboxPipeline`] wires together the four extension-point traits
//! ([`Storage`], [`DedupeStrategy`], [`SignatureVerifier`]) into
//! the four-stage ingest pipeline:
//!
//! ```text
//! Receive → Verify → Dedupe → Store (with delivery rows)
//! ```
//!
//! Stage 5 (inline emit) has been removed. After a successful store the
//! pipeline writes one `webhook_deliveries` row per configured emitter name
//! via [`Storage::store_with_deliveries`].  The `EmitterWorker` (in the
//! `hookbox-server` crate) picks up those rows and performs the actual
//! downstream emission.
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
use crate::traits::{DedupeStrategy, SignatureVerifier, Storage};
use crate::transitions;
use crate::types::WebhookReceipt;

/// Core pipeline that orchestrates webhook ingestion through four stages:
/// receive, verify, dedupe, and store (with delivery rows).
///
/// Generic over `S` (storage) and `D` (deduplication) to allow arbitrary
/// backend implementations.  The emitter type has been removed; downstream
/// forwarding is now handled by `EmitterWorker` in Phase 7.
pub struct HookboxPipeline<S, D> {
    storage: S,
    dedupe: D,
    emitter_names: Vec<String>,
    verifiers: HashMap<String, Box<dyn SignatureVerifier>>,
}

impl<S, D> HookboxPipeline<S, D>
where
    S: Storage,
    D: DedupeStrategy,
{
    /// Create a new [`HookboxPipelineBuilder`].
    #[must_use]
    pub fn builder() -> HookboxPipelineBuilder<S, D> {
        HookboxPipelineBuilder {
            storage: None,
            dedupe: None,
            emitter_names: Vec::new(),
            verifiers: HashMap::new(),
        }
    }

    /// Access the storage backend (e.g. for admin API queries).
    #[must_use]
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Returns the configured emitter names for this pipeline.
    #[must_use]
    pub fn emitter_names(&self) -> &[String] {
        &self.emitter_names
    }

    /// Ingest a single webhook event through the four-stage pipeline.
    ///
    /// Returns [`IngestResult::Accepted`] only after durable storage succeeds
    /// (receipt + delivery rows committed atomically).
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
                        "result" => transitions::LABEL_VERIFICATION_FAILED
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
                    "result" => transitions::LABEL_DEDUPE_FAILED
                )
                .increment(1);
                metrics::histogram!("hookbox_ingest_duration_seconds")
                    .record(ingest_start.elapsed().as_secs_f64());
                return Err(e.into());
            }
        };
        metrics::counter!(
            "hookbox_dedupe_checks_total",
            "provider" => provider.to_owned(),
            "result" => transitions::dedupe_decision_label(advisory)
        )
        .increment(1);
        if advisory == DedupeDecision::Duplicate {
            tracing::info!(
                %receipt_id, %provider, %dedupe_key,
                "advisory dedupe flagged duplicate, proceeding to authoritative check"
            );
        }

        // ── Stage 4: Store (build receipt + durable write with deliveries) ──
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
            parsed_payload,
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

        let store_start = Instant::now();
        let store_result = match self
            .storage
            .store_with_deliveries(&receipt, &self.emitter_names)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                metrics::counter!(
                    "hookbox_ingest_results_total",
                    "provider" => provider.to_owned(),
                    "result" => transitions::LABEL_STORE_FAILED
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
                    "result" => transitions::LABEL_DUPLICATE
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

        tracing::info!(%receipt_id, %provider, "webhook accepted");
        metrics::counter!(
            "hookbox_ingest_results_total",
            "provider" => provider.to_owned(),
            "result" => transitions::LABEL_ACCEPTED
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
/// [`build()`](HookboxPipelineBuilder::build) panics if `storage` or `dedupe`
/// have not been set.
pub struct HookboxPipelineBuilder<S, D> {
    storage: Option<S>,
    dedupe: Option<D>,
    emitter_names: Vec<String>,
    verifiers: HashMap<String, Box<dyn SignatureVerifier>>,
}

impl<S, D> HookboxPipelineBuilder<S, D>
where
    S: Storage,
    D: DedupeStrategy,
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

    /// Set the list of emitter names for which delivery rows will be created
    /// atomically alongside each new receipt.
    #[must_use]
    pub fn emitter_names(mut self, names: Vec<String>) -> Self {
        self.emitter_names = names;
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

    /// Register an already-boxed verifier.
    ///
    /// Useful when a caller builds a heterogeneous vector of verifiers from
    /// runtime configuration and cannot monomorphise over the concrete types
    /// at each call site (e.g. the server bootstrap path).
    #[must_use]
    pub fn verifier_boxed(mut self, v: Box<dyn SignatureVerifier>) -> Self {
        self.verifiers.insert(v.provider_name().to_owned(), v);
        self
    }

    /// Build the pipeline.
    ///
    /// # Panics
    ///
    /// Panics if `storage` or `dedupe` have not been set.
    #[expect(
        clippy::expect_used,
        reason = "builder panics by design when required fields are missing"
    )]
    #[must_use]
    pub fn build(self) -> HookboxPipeline<S, D> {
        HookboxPipeline {
            storage: self.storage.expect("storage must be set before build()"),
            dedupe: self.dedupe.expect("dedupe must be set before build()"),
            emitter_names: self.emitter_names,
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
    use crate::error::StorageError;
    use crate::state::{StoreResult, VerificationResult, VerificationStatus};
    use crate::traits::{SignatureVerifier, Storage};
    use crate::types::{ReceiptFilter, WebhookReceipt};

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

    /// Mock storage that records `store_with_deliveries` calls and asserts
    /// `store()` is never called directly by the pipeline (it has been replaced
    /// by `store_with_deliveries`).
    struct MockStorage {
        /// Recorded (`receipt_id`, `emitter_names`) pairs for each call.
        calls: Mutex<Vec<(Uuid, Vec<String>)>>,
        /// Whether to simulate a duplicate on the next call.
        return_duplicate: bool,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                return_duplicate: false,
            }
        }

        fn new_returning_duplicate() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                return_duplicate: true,
            }
        }

        fn recorded_calls(&self) -> Vec<(Uuid, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Storage for MockStorage {
        async fn store(&self, _receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
            panic!(
                "store() must not be called after the fan-out refactor — pipeline must use store_with_deliveries()"
            );
        }

        async fn get(&self, _id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
            Ok(None)
        }

        async fn update_state(
            &self,
            _id: Uuid,
            _state: ProcessingState,
            _error: Option<&str>,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
            Ok(vec![])
        }

        async fn store_with_deliveries(
            &self,
            receipt: &WebhookReceipt,
            emitter_names: &[String],
        ) -> Result<StoreResult, StorageError> {
            let mut calls = self
                .calls
                .lock()
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            calls.push((receipt.receipt_id.0, emitter_names.to_vec()));

            if self.return_duplicate {
                return Ok(StoreResult::Duplicate {
                    existing_id: receipt.receipt_id,
                });
            }
            Ok(StoreResult::Stored)
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

    // ── Tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ingest_accepted_on_valid_webhook() {
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(vec![])
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
            .emitter_names(vec![])
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
            .emitter_names(vec![])
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
            .emitter_names(vec![])
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

    /// Verifier that returns [`VerificationStatus::Failed`] with no reason string.
    struct FailVerifierNoReason;

    #[async_trait]
    impl SignatureVerifier for FailVerifierNoReason {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Failed,
                reason: None,
            }
        }
    }

    /// Verifier that returns [`VerificationStatus::Verified`] with no reason string.
    struct PassVerifierNoReason;

    #[async_trait]
    impl SignatureVerifier for PassVerifierNoReason {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Verified,
                reason: None,
            }
        }
    }

    /// Verifier that returns [`VerificationStatus::Skipped`] with no reason string.
    struct SkipVerifierNoReason;

    #[async_trait]
    impl SignatureVerifier for SkipVerifierNoReason {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Skipped,
                reason: None,
            }
        }
    }

    #[tokio::test]
    async fn ingest_verification_failed_with_no_reason() {
        // Covers the `unwrap_or_else(|| "verification_failed".to_owned())` branch.
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(vec![])
            .verifier(FailVerifierNoReason)
            .build();

        let body = Bytes::from(r#"{"event":"test"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        match result {
            Ok(IngestResult::VerificationFailed { reason }) => {
                assert_eq!(reason, "verification_failed");
            }
            other => panic!("expected VerificationFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ingest_verified_with_no_reason_label() {
        // Covers the `unwrap_or_else(|| "none".to_owned())` branch for Verified.
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(vec![])
            .verifier(PassVerifierNoReason)
            .build();

        let body = Bytes::from(r#"{"event":"test"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        assert!(
            matches!(result, Ok(IngestResult::Accepted { .. })),
            "expected Accepted, got {result:?}"
        );
    }

    #[tokio::test]
    async fn ingest_skipped_with_no_reason_label() {
        // Covers the `unwrap_or_else(|| "none".to_owned())` branch for Skipped.
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(vec![])
            .verifier(SkipVerifierNoReason)
            .build();

        let body = Bytes::from(r#"{"event":"test"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        assert!(
            matches!(result, Ok(IngestResult::Accepted { .. })),
            "expected Accepted for Skipped verification, got {result:?}"
        );
    }

    #[tokio::test]
    async fn memory_storage_get_returns_stored_receipt() {
        // Exercises MemoryStorage::get and MemoryStorage::query.
        let storage = MemoryStorage::new();
        let body = Bytes::from(r#"{"event":"test"}"#);
        let pipeline = HookboxPipeline::builder()
            .storage(storage)
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(vec![])
            .verifier(PassVerifier)
            .build();

        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await
            .unwrap();

        let IngestResult::Accepted { receipt_id } = result else {
            panic!("expected Accepted");
        };

        // Exercise get()
        let found = pipeline.storage().get(receipt_id.0).await.unwrap();
        assert!(found.is_some(), "get() should find the stored receipt");
        assert_eq!(found.unwrap().receipt_id, receipt_id);

        // Exercise query()
        let all = pipeline
            .storage()
            .query(ReceiptFilter::default())
            .await
            .unwrap();
        assert!(!all.is_empty(), "query() should return stored receipts");
    }

    /// Pipeline calls `store_with_deliveries` (not `store`) and passes the
    /// correct emitter names.
    #[tokio::test]
    async fn pipeline_calls_store_with_deliveries_with_correct_names() {
        let storage = MockStorage::new();
        let names = vec!["kafka".to_owned(), "sqs".to_owned()];

        let pipeline = HookboxPipeline::builder()
            .storage(storage)
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(names.clone())
            .build();

        let body = Bytes::from(r#"{"event":"fan-out-test"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        assert!(
            matches!(result, Ok(IngestResult::Accepted { .. })),
            "expected Accepted, got {result:?}"
        );

        let calls = pipeline.storage().recorded_calls();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one store_with_deliveries call"
        );
        assert_eq!(calls[0].1, names, "emitter names should be passed through");
    }

    /// When storage returns `Duplicate`, the pipeline short-circuits and does
    /// not insert delivery rows (that is the responsibility of the storage
    /// implementation, which we verify separately; here we just check the
    /// pipeline returns `Duplicate`).
    #[tokio::test]
    async fn pipeline_returns_duplicate_when_storage_reports_conflict() {
        let storage = MockStorage::new_returning_duplicate();

        let pipeline = HookboxPipeline::builder()
            .storage(storage)
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter_names(vec!["kafka".to_owned()])
            .build();

        let body = Bytes::from(r#"{"event":"duplicate-test"}"#);
        let result = pipeline
            .ingest("test-provider", HeaderMap::new(), body)
            .await;

        assert!(
            matches!(result, Ok(IngestResult::Duplicate { .. })),
            "expected Duplicate, got {result:?}"
        );

        // store_with_deliveries was still called once (the mock decides to return Duplicate).
        let calls = pipeline.storage().recorded_calls();
        assert_eq!(
            calls.len(),
            1,
            "store_with_deliveries should have been called once"
        );
    }

    /// Backends that do not override `store_with_deliveries` must reject any
    /// call that carries a non-empty `emitter_names` slice. Silently falling
    /// back to `store()` in that case would drop fan-out rows and make newly
    /// accepted webhooks invisible to every `EmitterWorker`.
    #[tokio::test]
    async fn default_store_with_deliveries_rejects_non_empty_emitter_names() {
        let storage = MemoryStorage::new();
        let receipt = WebhookReceipt {
            receipt_id: ReceiptId::new(),
            provider_name: "test".to_owned(),
            provider_event_id: None,
            external_reference: None,
            dedupe_key: "k".to_owned(),
            payload_hash: "h".to_owned(),
            raw_body: Vec::new(),
            parsed_payload: None,
            raw_headers: serde_json::json!({}),
            normalized_event_type: None,
            verification_status: VerificationStatus::Verified,
            verification_reason: None,
            processing_state: ProcessingState::Stored,
            emit_count: 0,
            last_error: None,
            received_at: chrono::Utc::now(),
            processed_at: None,
            metadata: serde_json::json!({}),
        };
        // Empty slice still delegates to `store` — backwards compatible.
        let ok = storage.store_with_deliveries(&receipt, &[]).await;
        assert!(matches!(ok, Ok(StoreResult::Stored)));
        // Non-empty slice must trip the default guard.
        let result = storage
            .store_with_deliveries(&receipt, &["kafka".to_owned()])
            .await;
        assert!(
            matches!(result, Err(StorageError::FanOutNotImplemented)),
            "expected Err(FanOutNotImplemented), got {result:?}",
        );
    }
}
