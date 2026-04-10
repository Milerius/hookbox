//! Cucumber BDD scenario tests for the hookbox ingest pipeline.
//!
//! Run with: `cargo test -p hookbox --test bdd`

#![allow(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
#![allow(clippy::panic, reason = "panic is acceptable in test assertions")]
#![allow(clippy::unused_async, reason = "cucumber step functions must be async")]
#![allow(clippy::len_zero, reason = "len >= N reads clearer in assertions")]
#![allow(missing_docs, reason = "test code does not require docs")]

use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use cucumber::{World, given, then, when};
use http::HeaderMap;
use tokio::sync::mpsc;
use uuid::Uuid;

use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::error::{EmitError, StorageError};
use hookbox::pipeline::HookboxPipeline;
use hookbox::state::{
    IngestResult, ProcessingState, StoreResult, VerificationResult, VerificationStatus,
};
use hookbox::traits::{Emitter, SignatureVerifier, Storage};
use hookbox::types::{NormalizedEvent, ReceiptFilter, WebhookReceipt};

// ── Test helpers ─────────────────────────────────────────────────────────

/// In-memory storage implementation for BDD tests.
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

/// Verifier that always passes, keyed by a configurable provider name.
struct PassVerifier {
    provider: String,
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

/// Verifier that always fails, keyed by a configurable provider name.
struct FailVerifier {
    provider: String,
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

/// Emitter that always fails.
struct FailEmitter;

#[async_trait]
impl Emitter for FailEmitter {
    async fn emit(&self, _event: &NormalizedEvent) -> Result<(), EmitError> {
        Err(EmitError::Downstream("test_failure".to_owned()))
    }
}

// ── Pipeline wrapper ─────────────────────────────────────────────────────

/// Enum to hold either a channel-emitting pipeline or a fail-emitting pipeline,
/// since the pipeline is generic over emitter type.
enum PipelineVariant {
    Channel(HookboxPipeline<MemoryStorage, InMemoryRecentDedupe, ChannelEmitter>),
    Fail(HookboxPipeline<MemoryStorage, InMemoryRecentDedupe, FailEmitter>),
}

impl PipelineVariant {
    async fn ingest(
        &self,
        provider: &str,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<IngestResult, hookbox::IngestError> {
        match self {
            PipelineVariant::Channel(p) => p.ingest(provider, headers, body).await,
            PipelineVariant::Fail(p) => p.ingest(provider, headers, body).await,
        }
    }
}

// ── World ────────────────────────────────────────────────────────────────

#[derive(Debug, Default, World)]
#[world(init = Self::new)]
struct IngestWorld {
    /// The pipeline variant is stored as an Option because World requires Default.
    /// We use a raw pointer trick via Box to erase the type.
    pipeline: Option<PipelineBox>,
    results: Vec<IngestResult>,
    emitter_rx: Option<mpsc::Receiver<NormalizedEvent>>,
}

/// Type-erased pipeline box so that `IngestWorld` can derive `Debug`.
struct PipelineBox(Box<dyn std::any::Any + Send>);

impl std::fmt::Debug for PipelineBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineBox").finish()
    }
}

impl IngestWorld {
    fn new() -> Self {
        Self {
            pipeline: None,
            results: Vec::new(),
            emitter_rx: None,
        }
    }

    fn set_channel_pipeline(
        &mut self,
        pipeline: HookboxPipeline<MemoryStorage, InMemoryRecentDedupe, ChannelEmitter>,
    ) {
        self.pipeline = Some(PipelineBox(Box::new(PipelineVariant::Channel(pipeline))));
    }

    fn set_fail_pipeline(
        &mut self,
        pipeline: HookboxPipeline<MemoryStorage, InMemoryRecentDedupe, FailEmitter>,
    ) {
        self.pipeline = Some(PipelineBox(Box::new(PipelineVariant::Fail(pipeline))));
    }

    fn pipeline(&self) -> &PipelineVariant {
        self.pipeline
            .as_ref()
            .and_then(|b| b.0.downcast_ref::<PipelineVariant>())
            .unwrap()
    }
}

// ── Step definitions ─────────────────────────────────────────────────────

#[given(expr = "a pipeline with a passing verifier for {string}")]
async fn given_pipeline_with_passing_verifier(world: &mut IngestWorld, provider: String) {
    let (emitter, rx) = ChannelEmitter::new(16);
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .verifier(PassVerifier {
            provider: provider.clone(),
        })
        .build();
    world.set_channel_pipeline(pipeline);
    world.emitter_rx = Some(rx);
}

#[given(expr = "a pipeline with a failing verifier for {string}")]
async fn given_pipeline_with_failing_verifier(world: &mut IngestWorld, provider: String) {
    let (emitter, rx) = ChannelEmitter::new(16);
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .verifier(FailVerifier {
            provider: provider.clone(),
        })
        .build();
    world.set_channel_pipeline(pipeline);
    world.emitter_rx = Some(rx);
}

#[given("a pipeline with no verifiers")]
async fn given_pipeline_with_no_verifiers(world: &mut IngestWorld) {
    let (emitter, rx) = ChannelEmitter::new(16);
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();
    world.set_channel_pipeline(pipeline);
    world.emitter_rx = Some(rx);
}

#[given(expr = "a pipeline with a passing verifier for {string} and a failing emitter")]
async fn given_pipeline_with_failing_emitter(world: &mut IngestWorld, provider: String) {
    let pipeline = HookboxPipeline::builder()
        .storage(MemoryStorage::new())
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(FailEmitter)
        .verifier(PassVerifier {
            provider: provider.clone(),
        })
        .build();
    world.set_fail_pipeline(pipeline);
}

#[when(expr = "I ingest a webhook from {string} with body {string}")]
async fn when_ingest_webhook(world: &mut IngestWorld, provider: String, body: String) {
    let result = world
        .pipeline()
        .ingest(&provider, HeaderMap::new(), Bytes::from(body))
        .await
        .unwrap();
    world.results.push(result);
}

#[then(expr = "the result should be {string}")]
async fn then_result_should_be(world: &mut IngestWorld, expected: String) {
    assert_eq!(world.results.len(), 1, "expected exactly one result");
    assert_result_matches(&world.results[0], &expected);
}

#[then(expr = "an event should be emitted with provider {string}")]
async fn then_event_emitted_with_provider(world: &mut IngestWorld, provider: String) {
    let rx = world.emitter_rx.as_mut().unwrap();
    let event = rx.try_recv().unwrap();
    assert_eq!(event.provider_name, provider);
}

#[then(expr = "the first result should be {string}")]
async fn then_first_result_should_be(world: &mut IngestWorld, expected: String) {
    assert!(
        world.results.len() >= 1,
        "expected at least one result, got {}",
        world.results.len()
    );
    assert_result_matches(&world.results[0], &expected);
}

#[then(expr = "the second result should be {string}")]
async fn then_second_result_should_be(world: &mut IngestWorld, expected: String) {
    assert!(
        world.results.len() >= 2,
        "expected at least two results, got {}",
        world.results.len()
    );
    assert_result_matches(&world.results[1], &expected);
}

/// Helper to assert an `IngestResult` matches a human-readable label.
fn assert_result_matches(result: &IngestResult, expected: &str) {
    match expected {
        "accepted" => {
            assert!(
                matches!(result, IngestResult::Accepted { .. }),
                "expected Accepted, got {result:?}"
            );
        }
        "duplicate" => {
            assert!(
                matches!(result, IngestResult::Duplicate { .. }),
                "expected Duplicate, got {result:?}"
            );
        }
        "verification_failed" => {
            assert!(
                matches!(result, IngestResult::VerificationFailed { .. }),
                "expected VerificationFailed, got {result:?}"
            );
        }
        other => panic!("unknown expected result: {other}"),
    }
}

// ── Main ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    IngestWorld::cucumber().run("tests/features/").await;
}
