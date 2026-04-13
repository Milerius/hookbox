//! Cucumber world types for hookbox BDD scenario suites.
//!
//! Provides the in-memory pipeline fixtures ([`IngestWorld`]) used by the
//! always-on `core_bdd` test binary, and the `#[cfg(feature = "bdd-server")]`
//! gated [`ServerWorld`] for HTTP-level scenarios.

use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use cucumber::World;
use http::HeaderMap;
use uuid::Uuid;

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::error::StorageError;
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{IngestResult, ProcessingState, StoreResult, VerificationResult, VerificationStatus};
use hookbox::traits::{SignatureVerifier, Storage};
use hookbox::types::{ReceiptFilter, WebhookReceipt};

// ── MemoryStorage ────────────────────────────────────────────────────────

/// In-memory [`Storage`] implementation for scenario tests.
///
/// Not thread-safe across async awaits for mutation, but adequate for
/// single-threaded BDD step execution.
pub struct MemoryStorage {
    receipts: Mutex<Vec<WebhookReceipt>>,
}

impl std::fmt::Debug for MemoryStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStorage").finish()
    }
}

impl MemoryStorage {
    /// Create an empty [`MemoryStorage`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            receipts: Mutex::new(Vec::new()),
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
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

// ── Verifiers ────────────────────────────────────────────────────────────

/// Signature verifier that always reports a passing (verified) result.
pub struct PassVerifier {
    /// Provider name this verifier is registered for.
    pub provider: String,
}

#[async_trait]
impl SignatureVerifier for PassVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Verified,
            reason: Some("signature_valid".to_owned()),
        }
    }
}

/// Signature verifier that always reports a failing result.
pub struct FailVerifier {
    /// Provider name this verifier is registered for.
    pub provider: String,
}

#[async_trait]
impl SignatureVerifier for FailVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Failed,
            reason: Some("invalid_signature".to_owned()),
        }
    }
}

// ── IngestWorld ──────────────────────────────────────────────────────────

/// Type alias for the concrete pipeline type used in BDD scenarios.
pub type ScenarioPipeline = HookboxPipeline<MemoryStorage, InMemoryRecentDedupe>;

/// Type-erased pipeline box so [`IngestWorld`] can derive [`std::fmt::Debug`].
struct PipelineBox(Box<dyn std::any::Any + Send>);

impl std::fmt::Debug for PipelineBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineBox").finish()
    }
}

/// Cucumber [`World`] for in-memory, no-database BDD scenarios.
///
/// Holds a [`ScenarioPipeline`] and accumulates [`IngestResult`]s across
/// scenario steps for assertion.
#[derive(Debug, Default, World)]
#[world(init = Self::new)]
pub struct IngestWorld {
    /// The pipeline under test (wrapped for dyn-compatibility with `Debug`).
    pipeline: Option<PipelineBox>,
    /// Results from each `ingest` call in the current scenario.
    pub results: Vec<IngestResult>,
}

impl IngestWorld {
    /// Create a fresh [`IngestWorld`] with no pipeline and no results.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pipeline: None,
            results: Vec::new(),
        }
    }

    /// Install a [`ScenarioPipeline`] into the world.
    pub fn set_pipeline(&mut self, pipeline: ScenarioPipeline) {
        self.pipeline = Some(PipelineBox(Box::new(pipeline)));
    }

    /// Borrow the installed pipeline.
    ///
    /// # Panics
    ///
    /// Panics if no pipeline has been installed via [`Self::set_pipeline`].
    #[must_use]
    #[expect(clippy::expect_used, reason = "panics by design when called before set_pipeline")]
    pub fn pipeline(&self) -> &ScenarioPipeline {
        self.pipeline
            .as_ref()
            .and_then(|b| b.0.downcast_ref::<ScenarioPipeline>())
            .expect("pipeline must be set before accessing it in a step")
    }

    /// Convenience: ingest a webhook and push the result.
    ///
    /// # Errors
    ///
    /// Propagates any [`hookbox::error::IngestError`] returned by the pipeline.
    ///
    /// # Panics
    ///
    /// Never panics in practice — the `expect` guards a `Vec::last()` call
    /// immediately after a `push`, so the vector is guaranteed non-empty.
    #[expect(clippy::expect_used, reason = "Vec::last after push is infallible")]
    pub async fn ingest(
        &mut self,
        provider: &str,
        body: impl Into<Bytes>,
    ) -> Result<&IngestResult, hookbox::error::IngestError> {
        let result = self
            .pipeline()
            .ingest(provider, HeaderMap::new(), body.into())
            .await?;
        self.results.push(result);
        Ok(self.results.last().expect("just pushed"))
    }
}

// ── ServerWorld ──────────────────────────────────────────────────────────

#[cfg(feature = "bdd-server")]
pub use server_world::ServerWorld;

#[cfg(feature = "bdd-server")]
mod server_world {
    use cucumber::World;

    /// Cucumber [`World`] for full-stack HTTP BDD scenarios.
    ///
    /// Manages a live `hookbox-server` instance backed by a testcontainer
    /// Postgres database and a `reqwest` HTTP client for exercising the
    /// `/webhook/:provider` endpoint end-to-end.
    #[derive(Debug, Default, World)]
    #[world(init = Self::new)]
    pub struct ServerWorld {
        /// Base URL of the running server (e.g. `http://127.0.0.1:PORT`).
        pub base_url: Option<String>,
    }

    impl ServerWorld {
        /// Create a fresh [`ServerWorld`].
        #[must_use]
        pub fn new() -> Self {
            Self { base_url: None }
        }
    }
}
