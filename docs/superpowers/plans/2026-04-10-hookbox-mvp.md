# Hookbox MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the complete hookbox MVP — a durable webhook inbox library and standalone service that verifies, deduplicates, persists, replays, and audits webhook events.

**Architecture:** Bottom-up build across 5 crates. Start with core domain types, traits, dedupe, and emitters (`hookbox`), then durable storage (`hookbox-postgres`) so the authoritative store exists early, then provider adapters (`hookbox-providers`), then pipeline orchestration (back in `hookbox`), then HTTP server (`hookbox-server`), then CLI (`hookbox-cli`). Each task produces compiling, tested code.

**Tech Stack:** Rust 2024, Tokio, Axum 0.8, SQLx 0.8 (Postgres), tracing, metrics 0.24, clap 4, sha2 0.11, hmac 0.13, subtle 2, thiserror 2, anyhow 1, bolero 0.13.

**Testing approach:** TDD where practical — write the test, see it fail, implement, see it pass, commit. Unit tests co-located with source. Integration tests in `integration-tests/`. Property tests in `hookbox-verify`.

**Spec reference:** `docs/superpowers/specs/2026-04-10-hookbox-design.md`

---

## File Map

### `crates/hookbox/src/`

| File | Responsibility |
|------|---------------|
| `lib.rs` | Crate root, re-exports |
| `types.rs` | `WebhookReceipt`, `NormalizedEvent`, newtypes (`ReceiptId`, `DedupeKey`, `PayloadHash`) |
| `state.rs` | `ProcessingState`, `VerificationStatus`, `VerificationResult`, `DedupeDecision`, `StoreResult`, `IngestResult` |
| `error.rs` | `StorageError`, `DedupeError`, `EmitError`, `IngestError`, `VerificationError` |
| `traits.rs` | `SignatureVerifier`, `Storage`, `DedupeStrategy`, `Emitter` |
| `dedupe.rs` | `InMemoryRecentDedupe`, `LayeredDedupe` |
| `emitter.rs` | `CallbackEmitter`, `ChannelEmitter` |
| `pipeline.rs` | `HookboxPipeline`, `HookboxPipelineBuilder` |
| `hash.rs` | `compute_payload_hash()` utility |

### `crates/hookbox-postgres/src/`

| File | Responsibility |
|------|---------------|
| `lib.rs` | Crate root, re-exports |
| `storage.rs` | `PostgresStorage` implementing `Storage` |
| `dedupe.rs` | `StorageDedupe` implementing `DedupeStrategy` |
| `migrations/` | SQLx migrations directory |
| `migrations/0001_create_webhook_receipts.sql` | Initial schema |

### `crates/hookbox-providers/src/`

| File | Responsibility |
|------|---------------|
| `lib.rs` | Crate root, re-exports, feature gates |
| `stripe.rs` | `StripeVerifier` |
| `generic_hmac.rs` | `GenericHmacVerifier` |

### `crates/hookbox-server/src/`

| File | Responsibility |
|------|---------------|
| `lib.rs` | Crate root, `HookboxServer`, startup wiring |
| `config.rs` | `HookboxConfig` (TOML deserialization) |
| `routes/mod.rs` | Router construction |
| `routes/ingest.rs` | `POST /webhooks/:provider` handler |
| `routes/health.rs` | `/healthz`, `/readyz`, `/metrics` |
| `routes/admin.rs` | `/api/receipts`, `/api/receipts/:id`, `/api/receipts/:id/replay`, `/api/dlq` |
| `metrics.rs` | Prometheus metric definitions and recording helpers |

### `crates/hookbox-cli/src/`

| File | Responsibility |
|------|---------------|
| `main.rs` | Entry point, clap dispatch |
| `commands/mod.rs` | Subcommand module |
| `commands/serve.rs` | `hookbox serve` |
| `commands/receipts.rs` | `hookbox receipts list/inspect/search` |
| `commands/replay.rs` | `hookbox replay` |
| `commands/dlq.rs` | `hookbox dlq list/inspect/retry` |

---

## Task 1: Core Domain Types

**Files:**
- Create: `crates/hookbox/src/types.rs`
- Create: `crates/hookbox/src/state.rs`
- Create: `crates/hookbox/src/hash.rs`
- Modify: `crates/hookbox/src/lib.rs`

### Steps

- [ ] **Step 1: Create `state.rs` with enums**

```rust
// crates/hookbox/src/state.rs

//! Processing states, verification status, and pipeline decision types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle state of a webhook receipt.
///
/// `Stored` is the durable acceptance boundary; all subsequent states
/// describe downstream delivery progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingState {
    /// Just received, not yet verified.
    Received,
    /// Signature verification passed.
    Verified,
    /// Signature verification failed.
    VerificationFailed,
    /// Identified as a duplicate.
    Duplicate,
    /// Durably persisted — the acceptance boundary.
    Stored,
    /// Downstream emission succeeded.
    Emitted,
    /// Downstream consumer confirmed processing.
    Processed,
    /// Downstream emission failed (will retry).
    EmitFailed,
    /// Retries exhausted, moved to dead-letter queue.
    DeadLettered,
    /// Manually replayed from DLQ.
    Replayed,
}

/// Result of signature verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Signature valid, timestamp within tolerance.
    Verified,
    /// Signature invalid or verification error.
    Failed,
    /// No verifier configured for this provider.
    Skipped,
}

/// Structured verification outcome with machine-readable reason.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    /// Pass/fail/skip status.
    pub status: VerificationStatus,
    /// Machine-readable reason (e.g. "signature_valid", "timestamp_expired").
    pub reason: Option<String>,
}

/// Advisory dedupe decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupeDecision {
    /// Not seen before.
    New,
    /// Same dedupe key and same payload hash.
    Duplicate,
    /// Same dedupe key but different payload hash.
    Conflict,
}

/// Authoritative result of a durable store attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreResult {
    /// Successfully stored.
    Stored,
    /// Duplicate detected by storage unique constraint.
    Duplicate {
        /// The receipt ID of the existing record.
        existing_id: Uuid,
    },
}

/// Result of the full ingest pipeline.
#[derive(Debug, Clone)]
pub enum IngestResult {
    /// Receipt accepted and durably stored.
    Accepted {
        /// The newly assigned receipt ID.
        receipt_id: Uuid,
    },
    /// Duplicate of an existing receipt.
    Duplicate {
        /// The receipt ID of the existing record.
        existing_id: Uuid,
    },
    /// Signature verification failed.
    VerificationFailed {
        /// Machine-readable reason.
        reason: String,
    },
}
```

- [ ] **Step 2: Create `hash.rs` with payload hash utility**

```rust
// crates/hookbox/src/hash.rs

//! Payload hashing utility.

use sha2::{Digest, Sha256};

/// Compute SHA-256 hash of raw body bytes, returned as hex string.
#[must_use]
pub fn compute_payload_hash(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let result = hasher.finalize();
    hex::encode(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_produces_known_hash() {
        let hash = compute_payload_hash(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn deterministic_for_same_input() {
        let body = b"hello webhook";
        assert_eq!(compute_payload_hash(body), compute_payload_hash(body));
    }

    #[test]
    fn different_input_produces_different_hash() {
        assert_ne!(
            compute_payload_hash(b"payload_a"),
            compute_payload_hash(b"payload_b")
        );
    }
}
```

- [ ] **Step 3: Create `types.rs` with domain types**

```rust
// crates/hookbox/src/types.rs

//! Core domain types for the hookbox webhook inbox.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::{ProcessingState, VerificationStatus};

/// Unique identifier for a webhook receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReceiptId(pub Uuid);

impl ReceiptId {
    /// Generate a new random receipt ID.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ReceiptId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ReceiptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The central entity — one per ingested webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookReceipt {
    /// Internal unique ID, assigned on receive.
    pub receipt_id: ReceiptId,
    /// Provider name (e.g. "stripe", "bvnk").
    pub provider_name: String,
    /// Provider's own event ID (e.g. "evt_1234").
    pub provider_event_id: Option<String>,
    /// Business reference (payment ID, order ID).
    pub external_reference: Option<String>,
    /// Computed key for deduplication (unique constraint in storage).
    pub dedupe_key: String,
    /// SHA-256 fingerprint of raw body bytes.
    pub payload_hash: String,
    /// Immutable original HTTP body bytes.
    #[serde(with = "serde_bytes_base64")]
    pub raw_body: Vec<u8>,
    /// Parsed JSON projection of raw_body (convenience, not authoritative).
    pub parsed_payload: Option<serde_json::Value>,
    /// Original HTTP headers.
    pub raw_headers: serde_json::Value,
    /// Canonical event type (e.g. "payment.completed").
    pub normalized_event_type: Option<String>,
    /// Verification pass/fail/skip.
    pub verification_status: VerificationStatus,
    /// Machine-readable verification reason.
    pub verification_reason: Option<String>,
    /// Current lifecycle state.
    pub processing_state: ProcessingState,
    /// Number of emission attempts.
    pub emit_count: i32,
    /// Last error from emit or processing failure.
    pub last_error: Option<String>,
    /// When hookbox received this webhook.
    pub received_at: DateTime<Utc>,
    /// When downstream emission succeeded.
    pub processed_at: Option<DateTime<Utc>>,
    /// Extensible key-value metadata.
    pub metadata: serde_json::Value,
}

/// Event emitted downstream after durable storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedEvent {
    /// Receipt ID for back-reference to storage.
    pub receipt_id: ReceiptId,
    /// Provider name.
    pub provider_name: String,
    /// Canonical event type.
    pub event_type: Option<String>,
    /// Business reference.
    pub external_reference: Option<String>,
    /// Parsed JSON projection (convenience, not raw bytes).
    pub parsed_payload: Option<serde_json::Value>,
    /// SHA-256 of the original raw body.
    pub payload_hash: String,
    /// When hookbox received the webhook.
    pub received_at: DateTime<Utc>,
    /// Extensible metadata.
    pub metadata: serde_json::Value,
}

/// Filter criteria for querying receipts.
#[derive(Debug, Clone, Default)]
pub struct ReceiptFilter {
    /// Filter by provider name.
    pub provider_name: Option<String>,
    /// Filter by processing state.
    pub processing_state: Option<ProcessingState>,
    /// Filter by external reference.
    pub external_reference: Option<String>,
    /// Filter by provider event ID.
    pub provider_event_id: Option<String>,
    /// Maximum number of results.
    pub limit: Option<i64>,
    /// Offset for pagination.
    pub offset: Option<i64>,
}

/// Serde helper for raw_body as base64 in JSON serialization.
mod serde_bytes_base64 {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize bytes as base64 string.
    ///
    /// # Errors
    ///
    /// Returns serialization error if the serializer fails.
    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        serializer.serialize_str(&encoded)
    }

    /// Deserialize bytes from base64 string.
    ///
    /// # Errors
    ///
    /// Returns deserialization error if the input is not valid base64.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use base64::Engine;
        let s = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_id_is_unique() {
        let a = ReceiptId::new();
        let b = ReceiptId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn receipt_id_display() {
        let id = ReceiptId(Uuid::nil());
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000000");
    }
}
```

- [ ] **Step 4: Add `base64` dependency to hookbox Cargo.toml**

Add to `[dependencies]` in `crates/hookbox/Cargo.toml`:

```toml
base64 = "0.22"
```

- [ ] **Step 5: Update `lib.rs` to wire modules**

```rust
// crates/hookbox/src/lib.rs

//! Hookbox — a durable webhook inbox.
//!
//! Core library providing traits, types, and pipeline orchestration
//! for webhook receipt, verification, deduplication, storage, and emission.

pub mod error;
pub mod hash;
pub mod state;
pub mod types;

pub use error::*;
pub use hash::compute_payload_hash;
pub use state::*;
pub use types::*;
```

- [ ] **Step 6: Create empty `error.rs` stub**

```rust
// crates/hookbox/src/error.rs

//! Error types for hookbox operations.
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p hookbox`

Expected: All tests in `hash.rs` and `types.rs` pass.

- [ ] **Step 8: Run lint**

Run: `cargo clippy -p hookbox --all-targets -- -D warnings`

Expected: No warnings or errors.

- [ ] **Step 9: Commit**

```bash
git add crates/hookbox/
git commit -m "feat(hookbox): add core domain types, enums, and payload hash

WebhookReceipt, NormalizedEvent, ReceiptFilter, ProcessingState,
VerificationStatus, DedupeDecision, StoreResult, IngestResult,
ReceiptId newtype, and compute_payload_hash utility."
```

---

## Task 2: Error Types

**Files:**
- Modify: `crates/hookbox/src/error.rs`

### Steps

- [ ] **Step 1: Implement error types**

```rust
// crates/hookbox/src/error.rs

//! Error types for hookbox operations.

use thiserror::Error;

/// Errors from the `Storage` trait.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Database connection or query failure.
    #[error("storage error: {0}")]
    Internal(String),
    /// Serialization/deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Errors from the `DedupeStrategy` trait.
#[derive(Debug, Error)]
pub enum DedupeError {
    /// Dedupe check failed.
    #[error("dedupe error: {0}")]
    Internal(String),
}

/// Errors from the `Emitter` trait.
#[derive(Debug, Error)]
pub enum EmitError {
    /// Downstream consumer rejected or unreachable.
    #[error("emit error: {0}")]
    Downstream(String),
    /// Emission timed out.
    #[error("emit timeout: {0}")]
    Timeout(String),
}

/// Errors from the `SignatureVerifier` trait.
#[derive(Debug, Error)]
pub enum VerificationError {
    /// Verification logic failed unexpectedly.
    #[error("verification error: {0}")]
    Internal(String),
}

/// Errors from the ingest pipeline.
#[derive(Debug, Error)]
pub enum IngestError {
    /// Storage layer failed.
    #[error("storage failure: {0}")]
    Storage(#[from] StorageError),
    /// Dedupe layer failed.
    #[error("dedupe failure: {0}")]
    Dedupe(#[from] DedupeError),
    /// Emit layer failed.
    #[error("emit failure: {0}")]
    Emit(#[from] EmitError),
    /// Verification logic failed (not a verification rejection — an internal error).
    #[error("verification failure: {0}")]
    Verification(#[from] VerificationError),
}
```

- [ ] **Step 2: Run lint and test**

Run: `cargo clippy -p hookbox --all-targets -- -D warnings && cargo test -p hookbox`

Expected: Clean.

- [ ] **Step 3: Commit**

```bash
git add crates/hookbox/src/error.rs
git commit -m "feat(hookbox): add typed error types for storage, dedupe, emit, and ingest"
```

---

## Task 3: Core Traits

**Files:**
- Create: `crates/hookbox/src/traits.rs`
- Modify: `crates/hookbox/src/lib.rs`

### Steps

- [ ] **Step 1: Create `traits.rs`**

```rust
// crates/hookbox/src/traits.rs

//! Core extension point traits.
//!
//! Four traits define the hookbox extension surface. All are `Send + Sync`
//! for safe use across async tasks.

use async_trait::async_trait;
use http::HeaderMap;
use uuid::Uuid;

use crate::error::{DedupeError, EmitError, StorageError};
use crate::state::{DedupeDecision, StoreResult, VerificationResult};
use crate::types::{NormalizedEvent, ReceiptFilter, WebhookReceipt};

/// Provider-specific webhook signature verification.
///
/// Implementations verify the authenticity of incoming webhooks
/// using provider-specific mechanisms (HMAC, timestamp tolerance, etc.).
#[async_trait]
pub trait SignatureVerifier: Send + Sync {
    /// The provider name this verifier handles (e.g. "stripe").
    fn provider_name(&self) -> &str;

    /// Verify the incoming request signature.
    ///
    /// Returns a structured result with status and machine-readable reason.
    async fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> VerificationResult;
}

/// Durable webhook receipt persistence.
///
/// `store()` is the authoritative dedupe answer — the Postgres unique
/// constraint on `dedupe_key` decides truth.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Store a new receipt. Returns `StoreResult::Duplicate` if the
    /// dedupe key already exists (authoritative duplicate detection).
    async fn store(
        &self,
        receipt: &WebhookReceipt,
    ) -> Result<StoreResult, StorageError>;

    /// Get a receipt by its internal ID.
    async fn get(
        &self,
        id: Uuid,
    ) -> Result<Option<WebhookReceipt>, StorageError>;

    /// Update the processing state of a receipt.
    async fn update_state(
        &self,
        id: Uuid,
        state: crate::state::ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError>;

    /// Query receipts by filter criteria.
    async fn query(
        &self,
        filter: ReceiptFilter,
    ) -> Result<Vec<WebhookReceipt>, StorageError>;
}

/// Advisory fast-path duplicate detection.
///
/// `DedupeStrategy` is advisory — the pipeline asks it for a hint before
/// attempting storage. `Storage::store()` returning `StoreResult::Duplicate`
/// is the authoritative final answer.
#[async_trait]
pub trait DedupeStrategy: Send + Sync {
    /// Check if this key/hash combination is a known duplicate.
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError>;

    /// Record a key/hash as seen (advisory, for fast-path caching).
    async fn record(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<(), DedupeError>;
}

/// Downstream event forwarding after durable storage.
///
/// Emit failure does NOT invalidate the stored receipt. The receipt
/// is durable and accepted; emission has its own retry/DLQ lifecycle.
#[async_trait]
pub trait Emitter: Send + Sync {
    /// Emit a normalized event downstream.
    async fn emit(
        &self,
        event: &NormalizedEvent,
    ) -> Result<(), EmitError>;
}
```

- [ ] **Step 2: Update `lib.rs` to include traits module**

Add to `crates/hookbox/src/lib.rs`:

```rust
pub mod traits;
pub use traits::*;
```

- [ ] **Step 3: Verify `async-trait` dependency exists in hookbox Cargo.toml**

Confirm `async-trait = "0.1"` is present in `crates/hookbox/Cargo.toml`. We use `async-trait` for MVP smoothness — native async fn in traits can replace it later.

- [ ] **Step 4: Run lint and test**

Run: `cargo clippy -p hookbox --all-targets -- -D warnings && cargo test -p hookbox`

Expected: Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox/
git commit -m "feat(hookbox): add core traits — SignatureVerifier, Storage, DedupeStrategy, Emitter

Uses async-trait for MVP smoothness. Four extension points, all Send + Sync."
```

---

## Task 4: Built-in Dedupe Implementations

**Files:**
- Create: `crates/hookbox/src/dedupe.rs`
- Modify: `crates/hookbox/src/lib.rs`

### Steps

- [ ] **Step 1: Write tests for `InMemoryRecentDedupe`**

```rust
// At the bottom of crates/hookbox/src/dedupe.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lru_new_key_returns_new() {
        let dedupe = InMemoryRecentDedupe::new(100);
        let decision = dedupe.check("key1", "hash1").await.unwrap();
        assert_eq!(decision, DedupeDecision::New);
    }

    #[tokio::test]
    async fn lru_same_key_same_hash_returns_duplicate() {
        let dedupe = InMemoryRecentDedupe::new(100);
        dedupe.record("key1", "hash1").await.unwrap();
        let decision = dedupe.check("key1", "hash1").await.unwrap();
        assert_eq!(decision, DedupeDecision::Duplicate);
    }

    #[tokio::test]
    async fn lru_same_key_different_hash_returns_conflict() {
        let dedupe = InMemoryRecentDedupe::new(100);
        dedupe.record("key1", "hash1").await.unwrap();
        let decision = dedupe.check("key1", "hash_different").await.unwrap();
        assert_eq!(decision, DedupeDecision::Conflict);
    }

    #[tokio::test]
    async fn lru_eviction_makes_key_new_again() {
        let dedupe = InMemoryRecentDedupe::new(2);
        dedupe.record("key1", "h1").await.unwrap();
        dedupe.record("key2", "h2").await.unwrap();
        dedupe.record("key3", "h3").await.unwrap();
        // key1 should have been evicted
        let decision = dedupe.check("key1", "h1").await.unwrap();
        assert_eq!(decision, DedupeDecision::New);
    }

    #[tokio::test]
    async fn layered_returns_fast_path_duplicate() {
        let fast = InMemoryRecentDedupe::new(100);
        let slow = InMemoryRecentDedupe::new(100);
        fast.record("key1", "hash1").await.unwrap();
        let layered = LayeredDedupe::new(fast, slow);
        let decision = layered.check("key1", "hash1").await.unwrap();
        assert_eq!(decision, DedupeDecision::Duplicate);
    }

    #[tokio::test]
    async fn layered_falls_through_to_authoritative() {
        let fast = InMemoryRecentDedupe::new(100);
        let slow = InMemoryRecentDedupe::new(100);
        slow.record("key1", "hash1").await.unwrap();
        let layered = LayeredDedupe::new(fast, slow);
        let decision = layered.check("key1", "hash1").await.unwrap();
        assert_eq!(decision, DedupeDecision::Duplicate);
    }

    #[tokio::test]
    async fn layered_new_in_both_returns_new() {
        let fast = InMemoryRecentDedupe::new(100);
        let slow = InMemoryRecentDedupe::new(100);
        let layered = LayeredDedupe::new(fast, slow);
        let decision = layered.check("key1", "hash1").await.unwrap();
        assert_eq!(decision, DedupeDecision::New);
    }
}
```

- [ ] **Step 2: Implement `InMemoryRecentDedupe` and `LayeredDedupe`**

```rust
// crates/hookbox/src/dedupe.rs

//! Built-in dedupe strategy implementations.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::DedupeError;
use crate::state::DedupeDecision;
use crate::traits::DedupeStrategy;

/// In-memory LRU dedupe cache. Advisory only — not authoritative.
///
/// Uses a simple `HashMap` with a capacity limit. When full, the oldest
/// entry is evicted. This is a fast-path optimization to avoid hitting
/// storage for recent duplicates.
pub struct InMemoryRecentDedupe {
    cache: Mutex<LruMap>,
}

struct LruMap {
    map: HashMap<String, String>,
    order: Vec<String>,
    capacity: usize,
}

impl InMemoryRecentDedupe {
    /// Create a new LRU dedupe cache with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: Mutex::new(LruMap {
                map: HashMap::with_capacity(capacity),
                order: Vec::with_capacity(capacity),
                capacity,
            }),
        }
    }
}

impl DedupeStrategy for InMemoryRecentDedupe {
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError> {
        let cache = self
            .cache
            .lock()
            .map_err(|e| DedupeError::Internal(e.to_string()))?;

        match cache.map.get(dedupe_key) {
            None => Ok(DedupeDecision::New),
            Some(existing_hash) if existing_hash == payload_hash => Ok(DedupeDecision::Duplicate),
            Some(_) => Ok(DedupeDecision::Conflict),
        }
    }

    async fn record(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<(), DedupeError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|e| DedupeError::Internal(e.to_string()))?;

        if cache.map.contains_key(dedupe_key) {
            return Ok(());
        }

        if cache.order.len() >= cache.capacity {
            if let Some(oldest) = cache.order.first().cloned() {
                cache.map.remove(&oldest);
                cache.order.remove(0);
            }
        }

        cache
            .map
            .insert(dedupe_key.to_owned(), payload_hash.to_owned());
        cache.order.push(dedupe_key.to_owned());

        Ok(())
    }
}

/// Composes a fast-path strategy with an authoritative strategy.
///
/// Checks the fast path first. If it says `New`, falls through to the
/// authoritative strategy. If either says `Duplicate` or `Conflict`,
/// returns that immediately.
pub struct LayeredDedupe<F, A> {
    fast: F,
    authoritative: A,
}

impl<F, A> LayeredDedupe<F, A> {
    /// Create a new layered dedupe with fast path and authoritative fallback.
    pub fn new(fast: F, authoritative: A) -> Self {
        Self { fast, authoritative }
    }
}

impl<F: DedupeStrategy, A: DedupeStrategy> DedupeStrategy for LayeredDedupe<F, A> {
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError> {
        let fast_result = self.fast.check(dedupe_key, payload_hash).await?;
        if fast_result != DedupeDecision::New {
            return Ok(fast_result);
        }
        self.authoritative.check(dedupe_key, payload_hash).await
    }

    async fn record(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<(), DedupeError> {
        self.fast.record(dedupe_key, payload_hash).await?;
        self.authoritative.record(dedupe_key, payload_hash).await
    }
}

// ... tests at bottom (from Step 1)
```

- [ ] **Step 3: Update `lib.rs`**

Add to `crates/hookbox/src/lib.rs`:

```rust
pub mod dedupe;
pub use dedupe::{InMemoryRecentDedupe, LayeredDedupe};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p hookbox`

Expected: All 7 dedupe tests pass.

- [ ] **Step 5: Run lint**

Run: `cargo clippy -p hookbox --all-targets -- -D warnings`

Expected: Clean.

- [ ] **Step 6: Commit**

```bash
git add crates/hookbox/
git commit -m "feat(hookbox): add InMemoryRecentDedupe and LayeredDedupe implementations

LRU cache for fast-path advisory dedupe. LayeredDedupe composes fast
path with authoritative fallback. Conflict detection for same key
but different payload hash."
```

---

## Task 5: Built-in Emitter Implementations

**Files:**
- Create: `crates/hookbox/src/emitter.rs`
- Modify: `crates/hookbox/src/lib.rs`

### Steps

- [ ] **Step 1: Write tests for emitters**

```rust
// At the bottom of crates/hookbox/src/emitter.rs

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReceiptId;
    use chrono::Utc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
        let count = Arc::new(AtomicU32::new(0));
        let count_clone = count.clone();
        let emitter = CallbackEmitter::new(move |_event| {
            let c = count_clone.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        emitter.emit(&test_event()).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn channel_emitter_sends_event() {
        let (emitter, mut rx) = ChannelEmitter::new(16);
        let event = test_event();
        emitter.emit(&event).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.provider_name, "test");
    }
}
```

- [ ] **Step 2: Implement `CallbackEmitter` and `ChannelEmitter`**

```rust
// crates/hookbox/src/emitter.rs

//! Built-in emitter implementations.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::error::EmitError;
use crate::traits::Emitter;
use crate::types::NormalizedEvent;

/// Emitter that calls an async callback function.
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
    /// Create a new callback emitter with the given handler function.
    pub fn new(handler: F) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }
}

impl<F, Fut> Emitter for CallbackEmitter<F, Fut>
where
    F: Fn(NormalizedEvent) -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), EmitError>> + Send,
{
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        (self.handler)(event.clone()).await
    }
}

/// Emitter that sends events to a tokio mpsc channel.
pub struct ChannelEmitter {
    tx: mpsc::Sender<NormalizedEvent>,
}

impl ChannelEmitter {
    /// Create a new channel emitter with the given buffer size.
    ///
    /// Returns the emitter and the receiving half of the channel.
    #[must_use]
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<NormalizedEvent>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self { tx }, rx)
    }
}

impl Emitter for ChannelEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        self.tx
            .send(event.clone())
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))
    }
}

// ... tests at bottom (from Step 1)
```

- [ ] **Step 3: Update `lib.rs`**

Add to `crates/hookbox/src/lib.rs`:

```rust
pub mod emitter;
pub use emitter::{CallbackEmitter, ChannelEmitter};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p hookbox`

Expected: All emitter tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox/
git commit -m "feat(hookbox): add CallbackEmitter and ChannelEmitter implementations

CallbackEmitter wraps an async closure. ChannelEmitter sends to a
tokio mpsc channel. Both implement the Emitter trait."
```

---

## Task 6: Pipeline Orchestration

**Files:**
- Create: `crates/hookbox/src/pipeline.rs`
- Modify: `crates/hookbox/src/lib.rs`

### Steps

- [ ] **Step 1: Write pipeline tests**

```rust
// At the bottom of crates/hookbox/src/pipeline.rs

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dedupe::InMemoryRecentDedupe;
    use crate::error::StorageError;
    use crate::state::{ProcessingState, StoreResult, VerificationResult, VerificationStatus};
    use crate::types::{ReceiptFilter, ReceiptId, WebhookReceipt};
    use http::HeaderMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    // -- In-memory storage for testing --
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

    impl Storage for MemoryStorage {
        async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
            let mut receipts = self.receipts.lock().map_err(|e| StorageError::Internal(e.to_string()))?;
            if let Some(existing) = receipts.iter().find(|r| r.dedupe_key == receipt.dedupe_key) {
                return Ok(StoreResult::Duplicate {
                    existing_id: existing.receipt_id.0,
                });
            }
            receipts.push(receipt.clone());
            Ok(StoreResult::Stored)
        }

        async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
            let receipts = self.receipts.lock().map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(receipts.iter().find(|r| r.receipt_id.0 == id).cloned())
        }

        async fn update_state(
            &self,
            id: Uuid,
            state: ProcessingState,
            error: Option<&str>,
        ) -> Result<(), StorageError> {
            let mut receipts = self.receipts.lock().map_err(|e| StorageError::Internal(e.to_string()))?;
            if let Some(receipt) = receipts.iter_mut().find(|r| r.receipt_id.0 == id) {
                receipt.processing_state = state;
                receipt.last_error = error.map(String::from);
            }
            Ok(())
        }

        async fn query(&self, _filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
            let receipts = self.receipts.lock().map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(receipts.clone())
        }
    }

    // -- Always-pass verifier --
    struct PassVerifier;
    impl SignatureVerifier for PassVerifier {
        fn provider_name(&self) -> &str {
            "test"
        }
        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Verified,
                reason: Some("test_pass".to_owned()),
            }
        }
    }

    // -- Always-fail verifier --
    struct FailVerifier;
    impl SignatureVerifier for FailVerifier {
        fn provider_name(&self) -> &str {
            "fail_provider"
        }
        async fn verify(&self, _headers: &HeaderMap, _body: &[u8]) -> VerificationResult {
            VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("signature_mismatch".to_owned()),
            }
        }
    }

    fn build_pipeline_pass() -> HookboxPipeline<MemoryStorage, InMemoryRecentDedupe, crate::emitter::ChannelEmitter> {
        let (emitter, _rx) = crate::emitter::ChannelEmitter::new(16);
        HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(emitter)
            .verifier(PassVerifier)
            .build()
    }

    #[tokio::test]
    async fn ingest_accepted_on_valid_webhook() {
        let pipeline = build_pipeline_pass();
        let result = pipeline
            .ingest("test", HeaderMap::new(), Bytes::from(r#"{"event":"test"}"#))
            .await
            .unwrap();
        assert!(matches!(result, IngestResult::Accepted { .. }));
    }

    #[tokio::test]
    async fn ingest_duplicate_on_same_body() {
        let pipeline = build_pipeline_pass();
        let body = Bytes::from(r#"{"event":"test"}"#);
        let first = pipeline
            .ingest("test", HeaderMap::new(), body.clone())
            .await
            .unwrap();
        assert!(matches!(first, IngestResult::Accepted { .. }));

        let second = pipeline
            .ingest("test", HeaderMap::new(), body)
            .await
            .unwrap();
        assert!(matches!(second, IngestResult::Duplicate { .. }));
    }

    #[tokio::test]
    async fn ingest_verification_failed() {
        let (emitter, _rx) = crate::emitter::ChannelEmitter::new(16);
        let pipeline = HookboxPipeline::builder()
            .storage(MemoryStorage::new())
            .dedupe(InMemoryRecentDedupe::new(100))
            .emitter(emitter)
            .verifier(FailVerifier)
            .build();

        let result = pipeline
            .ingest(
                "fail_provider",
                HeaderMap::new(),
                Bytes::from("body"),
            )
            .await
            .unwrap();
        assert!(matches!(result, IngestResult::VerificationFailed { .. }));
    }

    #[tokio::test]
    async fn ingest_skipped_verification_when_no_verifier() {
        let pipeline = build_pipeline_pass();
        // "unknown_provider" has no registered verifier — should skip verification
        let result = pipeline
            .ingest(
                "unknown_provider",
                HeaderMap::new(),
                Bytes::from("body"),
            )
            .await
            .unwrap();
        assert!(matches!(result, IngestResult::Accepted { .. }));
    }
}
```

- [ ] **Step 2: Implement `HookboxPipeline` and builder**

```rust
// crates/hookbox/src/pipeline.rs

//! Pipeline orchestration — the core ingest flow.

use std::collections::HashMap;

use bytes::Bytes;
use chrono::Utc;
use http::HeaderMap;
use tracing::{info, warn, instrument};
use uuid::Uuid;

use crate::error::IngestError;
use crate::hash::compute_payload_hash;
use crate::state::{
    DedupeDecision, IngestResult, ProcessingState, StoreResult,
    VerificationResult, VerificationStatus,
};
use crate::traits::{DedupeStrategy, Emitter, SignatureVerifier, Storage};
use crate::types::{NormalizedEvent, ReceiptId, WebhookReceipt};

/// The central ingest pipeline. Wires together verification, deduplication,
/// storage, and emission.
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
    /// Create a new pipeline builder.
    pub fn builder() -> HookboxPipelineBuilder<S, D, E> {
        HookboxPipelineBuilder {
            storage: None,
            dedupe: None,
            emitter: None,
            verifiers: HashMap::new(),
        }
    }

    /// Process an incoming webhook through the full pipeline:
    /// receive → verify → dedupe → store → emit.
    ///
    /// Returns `Accepted` only after durable store succeeds.
    /// Emit runs after accept — its failure is handled via state transitions.
    #[instrument(skip(self, headers, body), fields(provider = %provider))]
    pub async fn ingest(
        &self,
        provider: &str,
        headers: HeaderMap,
        body: Bytes,
    ) -> Result<IngestResult, IngestError> {
        let receipt_id = ReceiptId::new();
        let payload_hash = compute_payload_hash(&body);
        let dedupe_key = format!("{provider}:{payload_hash}");

        // 1. Verify
        let verification = if let Some(verifier) = self.verifiers.get(provider) {
            verifier.verify(&headers, &body).await
        } else {
            VerificationResult {
                status: VerificationStatus::Skipped,
                reason: Some("no_verifier_configured".to_owned()),
            }
        };

        if verification.status == VerificationStatus::Failed {
            warn!(
                receipt_id = %receipt_id,
                reason = ?verification.reason,
                "verification failed"
            );
            return Ok(IngestResult::VerificationFailed {
                reason: verification
                    .reason
                    .unwrap_or_else(|| "unknown".to_owned()),
            });
        }

        // 2. Advisory dedupe check
        let dedupe_decision = self.dedupe.check(&dedupe_key, &payload_hash).await?;
        if dedupe_decision == DedupeDecision::Duplicate {
            info!(receipt_id = %receipt_id, "advisory dedupe: duplicate");
            // Still attempt store for authoritative answer, but log early signal
        }

        // 3. Parse payload
        let parsed_payload = serde_json::from_slice(&body).ok();

        // 4. Build receipt
        let headers_json = headers_to_json(&headers);
        let receipt = WebhookReceipt {
            receipt_id,
            provider_name: provider.to_owned(),
            provider_event_id: None,
            external_reference: None,
            dedupe_key: dedupe_key.clone(),
            payload_hash: payload_hash.clone(),
            raw_body: body.to_vec(),
            parsed_payload: parsed_payload.clone(),
            raw_headers: headers_json,
            normalized_event_type: None,
            verification_status: verification.status,
            verification_reason: verification.reason,
            processing_state: ProcessingState::Stored,
            emit_count: 0,
            last_error: None,
            received_at: Utc::now(),
            processed_at: None,
            metadata: serde_json::json!({}),
        };

        // 5. Store durably (authoritative dedupe)
        let store_result = self.storage.store(&receipt).await?;
        match store_result {
            StoreResult::Duplicate { existing_id } => {
                info!(receipt_id = %receipt_id, existing_id = %existing_id, "authoritative dedupe: duplicate");
                return Ok(IngestResult::Duplicate { existing_id });
            }
            StoreResult::Stored => {
                // Record in advisory dedupe for future fast-path
                let _ = self.dedupe.record(&dedupe_key, &payload_hash).await;
            }
        }

        info!(receipt_id = %receipt_id, "receipt stored, emitting");

        // 6. Emit (failure does NOT invalidate acceptance)
        let event = NormalizedEvent {
            receipt_id,
            provider_name: provider.to_owned(),
            event_type: None,
            external_reference: None,
            parsed_payload,
            payload_hash,
            received_at: receipt.received_at,
            metadata: serde_json::json!({}),
        };

        if let Err(e) = self.emitter.emit(&event).await {
            warn!(receipt_id = %receipt_id, error = %e, "emit failed, marking as EmitFailed");
            let _ = self
                .storage
                .update_state(receipt_id.0, ProcessingState::EmitFailed, Some(&e.to_string()))
                .await;
        } else {
            let _ = self
                .storage
                .update_state(receipt_id.0, ProcessingState::Emitted, None)
                .await;
        }

        Ok(IngestResult::Accepted {
            receipt_id: receipt_id.0,
        })
    }
}

/// Builder for `HookboxPipeline`.
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
    pub fn storage(mut self, storage: S) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Set the dedupe strategy.
    #[must_use]
    pub fn dedupe(mut self, dedupe: D) -> Self {
        self.dedupe = Some(dedupe);
        self
    }

    /// Set the emitter.
    #[must_use]
    pub fn emitter(mut self, emitter: E) -> Self {
        self.emitter = Some(emitter);
        self
    }

    /// Register a signature verifier for a provider.
    #[must_use]
    pub fn verifier(mut self, verifier: impl SignatureVerifier + 'static) -> Self {
        self.verifiers
            .insert(verifier.provider_name().to_owned(), Box::new(verifier));
        self
    }

    /// Build the pipeline.
    ///
    /// # Panics
    ///
    /// Panics if storage, dedupe, or emitter are not set.
    #[must_use]
    pub fn build(self) -> HookboxPipeline<S, D, E> {
        HookboxPipeline {
            storage: self.storage.expect("storage is required"),
            dedupe: self.dedupe.expect("dedupe is required"),
            emitter: self.emitter.expect("emitter is required"),
            verifiers: self.verifiers,
        }
    }
}

fn headers_to_json(headers: &HeaderMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            map.insert(name.to_string(), serde_json::Value::String(v.to_owned()));
        }
    }
    serde_json::Value::Object(map)
}

// ... tests at bottom (from Step 1)
```

- [ ] **Step 3: Update `lib.rs`**

Add to `crates/hookbox/src/lib.rs`:

```rust
pub mod pipeline;
pub use pipeline::{HookboxPipeline, HookboxPipelineBuilder};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p hookbox`

Expected: All pipeline tests pass (accepted, duplicate, verification failed, skipped verification).

- [ ] **Step 5: Run lint**

Run: `cargo clippy -p hookbox --all-targets -- -D warnings`

Expected: Clean.

- [ ] **Step 6: Commit**

```bash
git add crates/hookbox/
git commit -m "feat(hookbox): add HookboxPipeline with builder pattern

Five-stage ingest pipeline: receive → verify → dedupe → store → emit.
ACK only after durable store. Emit failure does not invalidate acceptance.
Builder pattern for pipeline construction."
```

---

## Task 7: Generic HMAC Signature Verifier

**Files:**
- Create: `crates/hookbox-providers/src/generic_hmac.rs`
- Modify: `crates/hookbox-providers/src/lib.rs`

### Steps

- [ ] **Step 1: Write tests**

```rust
// At the bottom of crates/hookbox-providers/src/generic_hmac.rs

#[cfg(test)]
mod tests {
    use super::*;
    use hookbox::state::VerificationStatus;

    fn make_signature(secret: &[u8], body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn valid_signature_passes() {
        let secret = b"test-secret";
        let body = b"hello webhook";
        let sig = make_signature(secret, body);

        let verifier = GenericHmacVerifier::new(
            "test_provider",
            secret.to_vec(),
            "X-Webhook-Signature".to_owned(),
        );

        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Signature", sig.parse().unwrap());

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
    }

    #[tokio::test]
    async fn invalid_signature_fails() {
        let verifier = GenericHmacVerifier::new(
            "test_provider",
            b"test-secret".to_vec(),
            "X-Webhook-Signature".to_owned(),
        );

        let mut headers = HeaderMap::new();
        headers.insert("X-Webhook-Signature", "bad_sig".parse().unwrap());

        let result = verifier.verify(&headers, b"hello").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_header_fails() {
        let verifier = GenericHmacVerifier::new(
            "test_provider",
            b"secret".to_vec(),
            "X-Webhook-Signature".to_owned(),
        );

        let result = verifier.verify(&HeaderMap::new(), b"body").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }
}
```

- [ ] **Step 2: Implement `GenericHmacVerifier`**

```rust
// crates/hookbox-providers/src/generic_hmac.rs

//! Configurable HMAC-SHA256 signature verifier.

use hmac::{Hmac, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// A generic HMAC-SHA256 signature verifier.
///
/// Works with most webhook providers that sign the body with HMAC-SHA256
/// and send the hex-encoded signature in a configurable header.
pub struct GenericHmacVerifier {
    provider: String,
    secret: Vec<u8>,
    header_name: String,
}

impl GenericHmacVerifier {
    /// Create a new generic HMAC verifier.
    #[must_use]
    pub fn new(provider: &str, secret: Vec<u8>, header_name: String) -> Self {
        Self {
            provider: provider.to_owned(),
            secret,
            header_name,
        }
    }
}

impl SignatureVerifier for GenericHmacVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        let Some(sig_header) = headers.get(&self.header_name) else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("missing_signature_header".to_owned()),
            };
        };

        let Ok(sig_str) = sig_header.to_str() else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_signature_header_encoding".to_owned()),
            };
        };

        let Ok(provided_sig) = hex::decode(sig_str) else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_hex_signature".to_owned()),
            };
        };

        let Ok(mut mac) = HmacSha256::new_from_slice(&self.secret) else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_secret_length".to_owned()),
            };
        };

        mac.update(body);
        let expected = mac.finalize().into_bytes();

        if expected.ct_eq(&provided_sig).into() {
            VerificationResult {
                status: VerificationStatus::Verified,
                reason: Some("signature_valid".to_owned()),
            }
        } else {
            VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("signature_mismatch".to_owned()),
            }
        }
    }
}

// ... tests at bottom (from Step 1)
```

- [ ] **Step 3: Update `lib.rs`**

```rust
// crates/hookbox-providers/src/lib.rs

//! Provider signature verification adapters for hookbox.

#[cfg(feature = "generic-hmac")]
pub mod generic_hmac;

#[cfg(feature = "generic-hmac")]
pub use generic_hmac::GenericHmacVerifier;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p hookbox-providers`

Expected: All 3 generic HMAC tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hookbox-providers/
git commit -m "feat(hookbox-providers): add GenericHmacVerifier

Configurable HMAC-SHA256 verifier with constant-time signature comparison.
Configurable header name and signing secret."
```

---

## Task 8: Stripe Signature Verifier

**Files:**
- Create: `crates/hookbox-providers/src/stripe.rs`
- Modify: `crates/hookbox-providers/src/lib.rs`

### Steps

- [ ] **Step 1: Write tests**

```rust
// At the bottom of crates/hookbox-providers/src/stripe.rs

#[cfg(test)]
mod tests {
    use super::*;
    use hookbox::state::VerificationStatus;
    use std::time::Duration;

    fn sign_stripe(secret: &str, timestamp: i64, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let signed_payload = format!("{timestamp}.{}", std::str::from_utf8(body).unwrap());
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed_payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    #[tokio::test]
    async fn valid_stripe_signature() {
        let secret = "whsec_test_secret";
        let body = b"test body";
        let timestamp = Utc::now().timestamp();
        let sig = sign_stripe(secret, timestamp, body);

        let verifier = StripeVerifier::new(secret.to_owned())
            .with_tolerance(Duration::from_secs(300));

        let mut headers = HeaderMap::new();
        let header_value = format!("t={timestamp},v1={sig}");
        headers.insert("Stripe-Signature", header_value.parse().unwrap());

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Verified);
    }

    #[tokio::test]
    async fn expired_timestamp_fails() {
        let secret = "whsec_test_secret";
        let body = b"test body";
        let old_timestamp = Utc::now().timestamp() - 600;
        let sig = sign_stripe(secret, old_timestamp, body);

        let verifier = StripeVerifier::new(secret.to_owned())
            .with_tolerance(Duration::from_secs(300));

        let mut headers = HeaderMap::new();
        let header_value = format!("t={old_timestamp},v1={sig}");
        headers.insert("Stripe-Signature", header_value.parse().unwrap());

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("timestamp_expired"));
    }

    #[tokio::test]
    async fn wrong_secret_fails() {
        let body = b"test body";
        let timestamp = Utc::now().timestamp();
        let sig = sign_stripe("wrong_secret", timestamp, body);

        let verifier = StripeVerifier::new("real_secret".to_owned())
            .with_tolerance(Duration::from_secs(300));

        let mut headers = HeaderMap::new();
        let header_value = format!("t={timestamp},v1={sig}");
        headers.insert("Stripe-Signature", header_value.parse().unwrap());

        let result = verifier.verify(&headers, body).await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("signature_mismatch"));
    }

    #[tokio::test]
    async fn missing_stripe_header_fails() {
        let verifier = StripeVerifier::new("secret".to_owned())
            .with_tolerance(Duration::from_secs(300));
        let result = verifier.verify(&HeaderMap::new(), b"body").await;
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.reason.as_deref(), Some("missing_signature_header"));
    }
}
```

- [ ] **Step 2: Implement `StripeVerifier`**

```rust
// crates/hookbox-providers/src/stripe.rs

//! Stripe webhook signature verifier.
//!
//! Verifies the `Stripe-Signature` header using HMAC-SHA256 with
//! timestamp tolerance.

use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// Stripe webhook signature verifier.
///
/// Verifies the `Stripe-Signature` header format: `t=<timestamp>,v1=<signature>`.
/// Uses constant-time comparison and enforces a configurable timestamp tolerance.
pub struct StripeVerifier {
    secret: String,
    tolerance: Duration,
}

impl StripeVerifier {
    /// Create a new Stripe verifier with the given webhook signing secret.
    ///
    /// Default tolerance is 5 minutes (300 seconds).
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self {
            secret,
            tolerance: Duration::from_secs(300),
        }
    }

    /// Set the timestamp tolerance window.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }
}

impl SignatureVerifier for StripeVerifier {
    fn provider_name(&self) -> &str {
        "stripe"
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        let Some(sig_header) = headers.get("Stripe-Signature") else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("missing_signature_header".to_owned()),
            };
        };

        let Ok(sig_str) = sig_header.to_str() else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_signature_header_encoding".to_owned()),
            };
        };

        // Parse t=<timestamp>,v1=<signature>
        let mut timestamp: Option<i64> = None;
        let mut signature: Option<String> = None;

        for part in sig_str.split(',') {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            match key {
                "t" => timestamp = value.parse().ok(),
                "v1" => signature = Some(value.to_owned()),
                _ => {}
            }
        }

        let Some(ts) = timestamp else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("missing_timestamp".to_owned()),
            };
        };

        let Some(provided_sig_hex) = signature else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("missing_v1_signature".to_owned()),
            };
        };

        // Check timestamp tolerance
        let now = Utc::now().timestamp();
        let age = now.abs_diff(ts);
        if age > self.tolerance.as_secs() {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("timestamp_expired".to_owned()),
            };
        }

        // Compute expected signature
        let Ok(body_str) = std::str::from_utf8(body) else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("body_not_utf8".to_owned()),
            };
        };

        let signed_payload = format!("{ts}.{body_str}");
        let Ok(mut mac) = HmacSha256::new_from_slice(self.secret.as_bytes()) else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_secret".to_owned()),
            };
        };

        mac.update(signed_payload.as_bytes());
        let expected = mac.finalize().into_bytes();

        let Ok(provided_bytes) = hex::decode(&provided_sig_hex) else {
            return VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("invalid_hex_signature".to_owned()),
            };
        };

        if expected.ct_eq(&provided_bytes).into() {
            VerificationResult {
                status: VerificationStatus::Verified,
                reason: Some("signature_valid".to_owned()),
            }
        } else {
            VerificationResult {
                status: VerificationStatus::Failed,
                reason: Some("signature_mismatch".to_owned()),
            }
        }
    }
}

// ... tests at bottom (from Step 1)
```

- [ ] **Step 3: Add `chrono` dependency to hookbox-providers Cargo.toml**

Add to `[dependencies]` in `crates/hookbox-providers/Cargo.toml`:

```toml
chrono.workspace = true
```

- [ ] **Step 4: Update `lib.rs`**

Add to `crates/hookbox-providers/src/lib.rs`:

```rust
#[cfg(feature = "stripe")]
pub mod stripe;

#[cfg(feature = "stripe")]
pub use stripe::StripeVerifier;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p hookbox-providers`

Expected: All 7 provider tests pass (4 Stripe + 3 generic HMAC).

- [ ] **Step 6: Commit**

```bash
git add crates/hookbox-providers/
git commit -m "feat(hookbox-providers): add StripeVerifier

Stripe-Signature header verification with HMAC-SHA256, timestamp
tolerance, and constant-time comparison."
```

---

## Task 9: PostgreSQL Storage — Schema and Migrations

**Files:**
- Create: `crates/hookbox-postgres/migrations/0001_create_webhook_receipts.sql`
- Modify: `crates/hookbox-postgres/src/lib.rs`

### Steps

- [ ] **Step 1: Create migrations directory**

```bash
mkdir -p crates/hookbox-postgres/migrations
```

- [ ] **Step 2: Create initial migration**

```sql
-- crates/hookbox-postgres/migrations/0001_create_webhook_receipts.sql

CREATE TABLE IF NOT EXISTS webhook_receipts (
    receipt_id          UUID PRIMARY KEY,
    provider_name       TEXT NOT NULL,
    provider_event_id   TEXT,
    external_reference  TEXT,
    dedupe_key          TEXT NOT NULL,
    payload_hash        TEXT NOT NULL,
    raw_body            BYTEA NOT NULL,
    parsed_payload      JSONB,
    raw_headers         JSONB NOT NULL,
    normalized_event_type TEXT,
    verification_status TEXT NOT NULL,
    verification_reason TEXT,
    processing_state    TEXT NOT NULL,
    emit_count          INTEGER NOT NULL DEFAULT 0,
    last_error          TEXT,
    received_at         TIMESTAMPTZ NOT NULL,
    processed_at        TIMESTAMPTZ,
    metadata            JSONB NOT NULL DEFAULT '{}'
);

-- Authoritative duplicate detection
CREATE UNIQUE INDEX IF NOT EXISTS idx_webhook_receipts_dedupe_key
    ON webhook_receipts (dedupe_key);

-- Lookup by provider event
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_provider_event
    ON webhook_receipts (provider_name, provider_event_id);

-- Lookup by business reference
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_external_ref
    ON webhook_receipts (external_reference)
    WHERE external_reference IS NOT NULL;

-- Filter by lifecycle state
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_state
    ON webhook_receipts (processing_state);

-- Time-range queries, retention cleanup
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_received_at
    ON webhook_receipts (received_at);

-- Common compound query (e.g. "all failed Stripe receipts")
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_provider_state
    ON webhook_receipts (provider_name, processing_state);
```

- [ ] **Step 3: Update `lib.rs`**

```rust
// crates/hookbox-postgres/src/lib.rs

//! `PostgreSQL` storage backend for hookbox.

pub mod storage;
pub mod dedupe;

pub use storage::PostgresStorage;
pub use dedupe::StorageDedupe;
```

- [ ] **Step 4: Commit**

```bash
git add crates/hookbox-postgres/
git commit -m "feat(hookbox-postgres): add initial migration and module structure

Creates webhook_receipts table with all indexes specified in the design:
dedupe_key unique, provider+event_id, external_reference, processing_state,
received_at, and provider+state compound index."
```

---

## Task 10: PostgreSQL Storage Implementation

**Files:**
- Create: `crates/hookbox-postgres/src/storage.rs`
- Create: `crates/hookbox-postgres/src/dedupe.rs`

### Steps

- [ ] **Step 1: Implement `PostgresStorage`**

```rust
// crates/hookbox-postgres/src/storage.rs

//! `PostgreSQL` implementation of the `Storage` trait.

use hookbox::error::StorageError;
use hookbox::state::{ProcessingState, StoreResult};
use hookbox::types::{ReceiptFilter, ReceiptId, WebhookReceipt};
use hookbox::traits::Storage;
use sqlx::PgPool;
use uuid::Uuid;

/// `PostgreSQL`-backed durable storage for webhook receipts.
#[derive(Clone)]
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Create a new `PostgresStorage` with the given connection pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run pending database migrations.
    ///
    /// # Errors
    ///
    /// Returns error if migrations fail.
    pub async fn migrate(&self) -> Result<(), StorageError> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))
    }
}

impl Storage for PostgresStorage {
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        let verification_status = serde_json::to_string(&receipt.verification_status)
            .map_err(|e| StorageError::Serialization(e.to_string()))?
            .trim_matches('"')
            .to_owned();
        let processing_state = serde_json::to_string(&receipt.processing_state)
            .map_err(|e| StorageError::Serialization(e.to_string()))?
            .trim_matches('"')
            .to_owned();

        let result = sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO webhook_receipts (
                receipt_id, provider_name, provider_event_id, external_reference,
                dedupe_key, payload_hash, raw_body, parsed_payload, raw_headers,
                normalized_event_type, verification_status, verification_reason,
                processing_state, emit_count, last_error, received_at, processed_at, metadata
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT (dedupe_key) DO NOTHING
            RETURNING receipt_id
            "#,
        )
        .bind(receipt.receipt_id.0)
        .bind(&receipt.provider_name)
        .bind(&receipt.provider_event_id)
        .bind(&receipt.external_reference)
        .bind(&receipt.dedupe_key)
        .bind(&receipt.payload_hash)
        .bind(&receipt.raw_body)
        .bind(&receipt.parsed_payload)
        .bind(&receipt.raw_headers)
        .bind(&receipt.normalized_event_type)
        .bind(&verification_status)
        .bind(&receipt.verification_reason)
        .bind(&processing_state)
        .bind(receipt.emit_count)
        .bind(&receipt.last_error)
        .bind(receipt.received_at)
        .bind(receipt.processed_at)
        .bind(&receipt.metadata)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        match result {
            Some(_) => Ok(StoreResult::Stored),
            None => {
                // Conflict on dedupe_key — fetch existing receipt_id
                let existing_id = sqlx::query_scalar::<_, Uuid>(
                    "SELECT receipt_id FROM webhook_receipts WHERE dedupe_key = $1",
                )
                .bind(&receipt.dedupe_key)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

                Ok(StoreResult::Duplicate { existing_id })
            }
        }
    }

    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        let row = sqlx::query_as::<_, ReceiptRow>(
            "SELECT * FROM webhook_receipts WHERE receipt_id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        row.map(receipt_from_row).transpose()
    }

    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let state_str = serde_json::to_string(&state)
            .map_err(|e| StorageError::Serialization(e.to_string()))?
            .trim_matches('"')
            .to_owned();

        sqlx::query(
            r#"
            UPDATE webhook_receipts
            SET processing_state = $2, last_error = $3
            WHERE receipt_id = $1
            "#,
        )
        .bind(id)
        .bind(&state_str)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        Ok(())
    }

    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        let mut query = String::from("SELECT * FROM webhook_receipts WHERE 1=1");
        let mut args: Vec<String> = Vec::new();

        if let Some(ref provider) = filter.provider_name {
            args.push(provider.clone());
            query.push_str(&format!(" AND provider_name = ${}", args.len()));
        }

        if let Some(ref state) = filter.processing_state {
            let state_str = serde_json::to_string(state)
                .map_err(|e| StorageError::Serialization(e.to_string()))?
                .trim_matches('"')
                .to_owned();
            args.push(state_str);
            query.push_str(&format!(" AND processing_state = ${}", args.len()));
        }

        if let Some(ref ext_ref) = filter.external_reference {
            args.push(ext_ref.clone());
            query.push_str(&format!(" AND external_reference = ${}", args.len()));
        }

        if let Some(ref event_id) = filter.provider_event_id {
            args.push(event_id.clone());
            query.push_str(&format!(" AND provider_event_id = ${}", args.len()));
        }

        query.push_str(" ORDER BY received_at DESC");

        if let Some(limit) = filter.limit {
            query.push_str(&format!(" LIMIT {limit}"));
        }

        if let Some(offset) = filter.offset {
            query.push_str(&format!(" OFFSET {offset}"));
        }

        // Build the query dynamically
        let mut sqlx_query = sqlx::query_as::<_, ReceiptRow>(&query);
        for arg in &args {
            sqlx_query = sqlx_query.bind(arg);
        }

        let rows = sqlx_query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        rows.into_iter().map(receipt_from_row).collect()
    }
}

#[derive(sqlx::FromRow)]
struct ReceiptRow {
    receipt_id: Uuid,
    provider_name: String,
    provider_event_id: Option<String>,
    external_reference: Option<String>,
    dedupe_key: String,
    payload_hash: String,
    raw_body: Vec<u8>,
    parsed_payload: Option<serde_json::Value>,
    raw_headers: serde_json::Value,
    normalized_event_type: Option<String>,
    verification_status: String,
    verification_reason: Option<String>,
    processing_state: String,
    emit_count: i32,
    last_error: Option<String>,
    received_at: chrono::DateTime<chrono::Utc>,
    processed_at: Option<chrono::DateTime<chrono::Utc>>,
    metadata: serde_json::Value,
}

fn receipt_from_row(row: ReceiptRow) -> Result<WebhookReceipt, StorageError> {
    let verification_status = serde_json::from_str::<hookbox::state::VerificationStatus>(
        &format!("\"{}\"", row.verification_status),
    )
    .map_err(|e| StorageError::Serialization(e.to_string()))?;

    let processing_state = serde_json::from_str::<hookbox::state::ProcessingState>(
        &format!("\"{}\"", row.processing_state),
    )
    .map_err(|e| StorageError::Serialization(e.to_string()))?;

    Ok(WebhookReceipt {
        receipt_id: ReceiptId(row.receipt_id),
        provider_name: row.provider_name,
        provider_event_id: row.provider_event_id,
        external_reference: row.external_reference,
        dedupe_key: row.dedupe_key,
        payload_hash: row.payload_hash,
        raw_body: row.raw_body,
        parsed_payload: row.parsed_payload,
        raw_headers: row.raw_headers,
        normalized_event_type: row.normalized_event_type,
        verification_status,
        verification_reason: row.verification_reason,
        processing_state,
        emit_count: row.emit_count,
        last_error: row.last_error,
        received_at: row.received_at,
        processed_at: row.processed_at,
        metadata: row.metadata,
    })
}
```

- [ ] **Step 2: Implement `StorageDedupe`**

```rust
// crates/hookbox-postgres/src/dedupe.rs

//! Storage-backed advisory dedupe strategy.

use hookbox::error::DedupeError;
use hookbox::state::DedupeDecision;
use hookbox::traits::DedupeStrategy;
use sqlx::PgPool;

/// Advisory dedupe strategy backed by the `PostgreSQL` webhook_receipts table.
///
/// Queries the existing receipts to provide a fast advisory check before
/// the authoritative `Storage::store()` call.
#[derive(Clone)]
pub struct StorageDedupe {
    pool: PgPool,
}

impl StorageDedupe {
    /// Create a new storage-backed dedupe strategy.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl DedupeStrategy for StorageDedupe {
    async fn check(
        &self,
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Result<DedupeDecision, DedupeError> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT payload_hash FROM webhook_receipts WHERE dedupe_key = $1",
        )
        .bind(dedupe_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DedupeError::Internal(e.to_string()))?;

        match row {
            None => Ok(DedupeDecision::New),
            Some((existing_hash,)) if existing_hash == payload_hash => {
                Ok(DedupeDecision::Duplicate)
            }
            Some(_) => Ok(DedupeDecision::Conflict),
        }
    }

    async fn record(
        &self,
        _dedupe_key: &str,
        _payload_hash: &str,
    ) -> Result<(), DedupeError> {
        // No-op: the authoritative store handles persistence.
        // This adapter is read-only for advisory checks.
        Ok(())
    }
}
```

- [ ] **Step 3: Run lint**

Run: `cargo clippy -p hookbox-postgres --all-targets -- -D warnings`

Expected: Clean (no unit tests — storage requires Postgres, tested via integration tests later).

- [ ] **Step 4: Commit**

```bash
git add crates/hookbox-postgres/
git commit -m "feat(hookbox-postgres): implement PostgresStorage and StorageDedupe

PostgresStorage implements Storage trait with INSERT ON CONFLICT for
authoritative dedupe. StorageDedupe provides advisory fast-path checks.
ReceiptRow mapping with serde-based enum serialization."
```

---

## Task 11: Server Configuration and Routes

**Files:**
- Create: `crates/hookbox-server/src/config.rs`
- Create: `crates/hookbox-server/src/routes/mod.rs`
- Create: `crates/hookbox-server/src/routes/ingest.rs`
- Create: `crates/hookbox-server/src/routes/health.rs`
- Create: `crates/hookbox-server/src/routes/admin.rs`
- Create: `crates/hookbox-server/src/metrics.rs`
- Modify: `crates/hookbox-server/src/lib.rs`

This is a larger task. The implementing agent should break it into subtasks:
1. Config struct + TOML deserialization
2. Health routes (`/healthz`, `/readyz`, `/metrics`)
3. Ingest route (`POST /webhooks/:provider`)
4. Admin routes (`/api/receipts`, `/api/receipts/:id`, `/api/receipts/:id/replay`, `/api/dlq`)
5. Metrics definitions
6. Server startup wiring in `lib.rs`

### Steps

- [ ] **Step 1: Create `config.rs`**

```rust
// crates/hookbox-server/src/config.rs

//! Server configuration.

use std::collections::HashMap;
use serde::Deserialize;

/// Top-level server configuration, loaded from TOML.
#[derive(Debug, Deserialize)]
pub struct HookboxConfig {
    /// Server settings.
    pub server: ServerConfig,
    /// Database settings.
    pub database: DatabaseConfig,
    /// Provider configurations, keyed by provider name.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Dedupe settings.
    #[serde(default)]
    pub dedupe: DedupeConfig,
    /// Admin API settings.
    #[serde(default)]
    pub admin: AdminConfig,
}

/// HTTP server settings.
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// Bind host (default: "0.0.0.0").
    #[serde(default = "default_host")]
    pub host: String,
    /// Bind port (default: 8080).
    #[serde(default = "default_port")]
    pub port: u16,
    /// Maximum request body size in bytes (default: 1MB).
    #[serde(default = "default_body_limit")]
    pub body_limit: usize,
}

/// Database settings.
#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL.
    pub url: String,
    /// Maximum number of connections in the pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

/// Provider-specific configuration.
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    /// Provider type for verifier selection.
    #[serde(default = "default_provider_type")]
    pub r#type: String,
    /// Signing secret.
    pub secret: String,
    /// Header name for signature (for generic HMAC).
    pub header: Option<String>,
    /// Timestamp tolerance in seconds (for Stripe).
    pub tolerance_seconds: Option<u64>,
}

/// Dedupe settings.
#[derive(Debug, Deserialize, Default)]
pub struct DedupeConfig {
    /// LRU cache capacity (default: 10000).
    #[serde(default = "default_lru_capacity")]
    pub lru_capacity: usize,
}

/// Admin API settings.
#[derive(Debug, Deserialize, Default)]
pub struct AdminConfig {
    /// Bearer token for admin API authentication.
    pub bearer_token: Option<String>,
}

fn default_host() -> String { "0.0.0.0".to_owned() }
fn default_port() -> u16 { 8080 }
fn default_body_limit() -> usize { 1_048_576 }
fn default_max_connections() -> u32 { 10 }
fn default_lru_capacity() -> usize { 10_000 }
fn default_provider_type() -> String { "hmac-sha256".to_owned() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[server]
port = 9090

[database]
url = "postgres://localhost/hookbox"
"#;
        let config: HookboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.server.port, 9090);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.database.max_connections, 10);
        assert_eq!(config.dedupe.lru_capacity, 10_000);
    }

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 8080
body_limit = 2097152

[database]
url = "postgres://localhost/hookbox"
max_connections = 20

[providers.stripe]
type = "stripe"
secret = "whsec_test"
tolerance_seconds = 300

[providers.bvnk]
type = "hmac-sha256"
secret = "bvnk_secret"
header = "X-Webhook-Signature"

[dedupe]
lru_capacity = 50000

[admin]
bearer_token = "super-secret-token"
"#;
        let config: HookboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.providers.len(), 2);
        assert!(config.providers.contains_key("stripe"));
        assert_eq!(config.admin.bearer_token.as_deref(), Some("super-secret-token"));
    }
}
```

- [ ] **Step 2: Create health routes**

```rust
// crates/hookbox-server/src/routes/health.rs

//! Health and readiness endpoints.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::PgPool;

/// Liveness probe — always returns 200.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — checks database connectivity.
pub async fn readyz(State(pool): State<PgPool>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(&pool).await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
```

- [ ] **Step 3: Create ingest route**

```rust
// crates/hookbox-server/src/routes/ingest.rs

//! Webhook ingest endpoint.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use hookbox::state::IngestResult;

use crate::AppState;

/// `POST /webhooks/:provider` — ingest a webhook.
pub async fn ingest_webhook(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    match state.pipeline.ingest(&provider, headers, body).await {
        Ok(IngestResult::Accepted { receipt_id }) => {
            (StatusCode::OK, Json(json!({ "status": "accepted", "receipt_id": receipt_id.to_string() })))
        }
        Ok(IngestResult::Duplicate { existing_id }) => {
            (StatusCode::OK, Json(json!({ "status": "duplicate", "existing_id": existing_id.to_string() })))
        }
        Ok(IngestResult::VerificationFailed { reason }) => {
            (StatusCode::UNAUTHORIZED, Json(json!({ "status": "verification_failed", "reason": reason })))
        }
        Err(e) => {
            tracing::error!(error = %e, "ingest error");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "status": "error", "message": e.to_string() })))
        }
    }
}
```

- [ ] **Step 4: Create admin routes**

```rust
// crates/hookbox-server/src/routes/admin.rs

//! Admin API endpoints.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use hookbox::state::ProcessingState;
use hookbox::traits::Storage;
use hookbox::types::ReceiptFilter;

use crate::AppState;

/// Query parameters for listing receipts.
#[derive(Debug, Deserialize, Default)]
pub struct ListReceiptsQuery {
    /// Filter by provider name.
    pub provider: Option<String>,
    /// Filter by processing state.
    pub state: Option<String>,
    /// Maximum number of results.
    pub limit: Option<i64>,
    /// Offset for pagination.
    pub offset: Option<i64>,
}

/// `GET /api/receipts` — list receipts with optional filters.
pub async fn list_receipts(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListReceiptsQuery>,
) -> impl IntoResponse {
    let processing_state = query.state.as_deref().and_then(|s| {
        serde_json::from_str::<ProcessingState>(&format!("\"{s}\"")).ok()
    });

    let filter = ReceiptFilter {
        provider_name: query.provider,
        processing_state,
        limit: query.limit.or(Some(50)),
        offset: query.offset,
        ..Default::default()
    };

    match state.pipeline.storage().query(filter).await {
        Ok(receipts) => (StatusCode::OK, Json(json!({ "receipts": receipts, "count": receipts.len() }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    }
}

/// `GET /api/receipts/:id` — inspect one receipt.
pub async fn get_receipt(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match state.pipeline.storage().get(id).await {
        Ok(Some(receipt)) => (StatusCode::OK, Json(json!(receipt))).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// `POST /api/receipts/:id/replay` — re-emit a receipt.
pub async fn replay_receipt(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let receipt = match state.pipeline.storage().get(id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    };

    let event = hookbox::types::NormalizedEvent {
        receipt_id: receipt.receipt_id,
        provider_name: receipt.provider_name,
        event_type: receipt.normalized_event_type,
        external_reference: receipt.external_reference,
        parsed_payload: receipt.parsed_payload,
        payload_hash: receipt.payload_hash,
        received_at: receipt.received_at,
        metadata: receipt.metadata,
    };

    match state.pipeline.emitter().emit(&event).await {
        Ok(()) => {
            let _ = state.pipeline.storage().update_state(id, ProcessingState::Replayed, None).await;
            (StatusCode::OK, Json(json!({ "status": "replayed", "receipt_id": id.to_string() }))).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// `GET /api/dlq` — list dead-lettered receipts.
pub async fn list_dlq(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListReceiptsQuery>,
) -> impl IntoResponse {
    let filter = ReceiptFilter {
        provider_name: query.provider,
        processing_state: Some(ProcessingState::DeadLettered),
        limit: query.limit.or(Some(50)),
        offset: query.offset,
        ..Default::default()
    };

    match state.pipeline.storage().query(filter).await {
        Ok(receipts) => (StatusCode::OK, Json(json!({ "receipts": receipts, "count": receipts.len() }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    }
}
```

- [ ] **Step 5: Create routes module**

```rust
// crates/hookbox-server/src/routes/mod.rs

//! HTTP route definitions.

pub mod admin;
pub mod health;
pub mod ingest;
```

- [ ] **Step 6: Create `metrics.rs` placeholder**

```rust
// crates/hookbox-server/src/metrics.rs

//! Prometheus metric definitions.
//!
//! Metrics are recorded at each pipeline stage. This module defines the
//! metric names and provides recording helpers.

// TODO: Implement metric definitions and recording helpers.
// For MVP, metrics are exposed via the metrics-exporter-prometheus crate
// and tracing provides structured logging at each pipeline step.
```

- [ ] **Step 7: Wire up `lib.rs` with server startup**

This step requires adding a `storage()` and `emitter()` accessor to `HookboxPipeline` in the core crate first. The implementing agent should:

1. Add `pub fn storage(&self) -> &S` and `pub fn emitter(&self) -> &E` methods to `HookboxPipeline` in `crates/hookbox/src/pipeline.rs`.
2. Then wire up the server `lib.rs`:

```rust
// crates/hookbox-server/src/lib.rs

//! Standalone Axum HTTP server for hookbox.

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;

use hookbox::dedupe::{InMemoryRecentDedupe, LayeredDedupe};
use hookbox::emitter::ChannelEmitter;
use hookbox::HookboxPipeline;
use hookbox_postgres::{PostgresStorage, StorageDedupe};

pub mod config;
pub mod metrics;
pub mod routes;

/// Shared application state.
pub struct AppState {
    /// The ingest pipeline.
    pub pipeline: HookboxPipeline<PostgresStorage, LayeredDedupe<InMemoryRecentDedupe, StorageDedupe>, ChannelEmitter>,
    /// Database connection pool (for health checks).
    pub pool: PgPool,
}

/// Build the Axum router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Webhook ingest
        .route("/webhooks/{provider}", post(routes::ingest::ingest_webhook))
        // Health
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        // Admin API
        .route("/api/receipts", get(routes::admin::list_receipts))
        .route("/api/receipts/{id}", get(routes::admin::get_receipt))
        .route("/api/receipts/{id}/replay", post(routes::admin::replay_receipt))
        .route("/api/dlq", get(routes::admin::list_dlq))
        .with_state(state)
        // Also provide pool directly for readyz
        .route("/readyz", get(routes::health::readyz).with_state(state.pool.clone()))
}
```

Note: The implementing agent will need to adjust the router construction to properly handle the dual state types (Arc<AppState> for most routes, PgPool for readyz). The simplest approach is to include `pool` inside `AppState` and extract it in the `readyz` handler via `State(state): State<Arc<AppState>>` then accessing `state.pool`.

- [ ] **Step 8: Run lint**

Run: `cargo clippy -p hookbox-server --all-targets -- -D warnings`

Expected: Clean (may need adjustments — the implementing agent should fix any issues).

- [ ] **Step 9: Commit**

```bash
git add crates/hookbox-server/ crates/hookbox/src/pipeline.rs
git commit -m "feat(hookbox-server): add Axum server with ingest, health, and admin routes

Config loading from TOML, POST /webhooks/:provider ingest endpoint,
/healthz and /readyz health probes, admin API for receipts and DLQ,
server startup wiring."
```

---

## Task 12: CLI Binary

**Files:**
- Modify: `crates/hookbox-cli/src/main.rs`
- Create: `crates/hookbox-cli/src/commands/mod.rs`
- Create: `crates/hookbox-cli/src/commands/serve.rs`

### Steps

- [ ] **Step 1: Implement `main.rs` with clap**

```rust
// crates/hookbox-cli/src/main.rs

//! Hookbox CLI — inspect, replay, and serve.

use clap::{Parser, Subcommand};

mod commands;

/// Hookbox — a durable webhook inbox.
#[derive(Parser)]
#[command(name = "hookbox", version, about)]
struct Cli {
    /// Path to configuration file.
    #[arg(short, long, default_value = "hookbox.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the webhook ingestion server.
    Serve,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve => commands::serve::run(&cli.config),
    }
}
```

- [ ] **Step 2: Create `commands/mod.rs`**

```rust
// crates/hookbox-cli/src/commands/mod.rs

//! CLI subcommands.

pub mod serve;
```

- [ ] **Step 3: Create `commands/serve.rs`**

```rust
// crates/hookbox-cli/src/commands/serve.rs

//! `hookbox serve` command — starts the webhook ingestion server.

use std::sync::Arc;

use anyhow::Context;

/// Run the hookbox server.
pub fn run(config_path: &str) -> anyhow::Result<()> {
    let config_str = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config file: {config_path}"))?;

    let config: hookbox_server::config::HookboxConfig = toml::from_str(&config_str)
        .context("failed to parse config")?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?
        .block_on(async { run_server(config).await })
}

async fn run_server(config: hookbox_server::config::HookboxConfig) -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    tracing::info!("starting hookbox server on {}:{}", config.server.host, config.server.port);

    // Connect to database
    let pool = sqlx::PgPool::connect(&config.database.url)
        .await
        .context("failed to connect to database")?;

    // Run migrations
    let storage = hookbox_postgres::PostgresStorage::new(pool.clone());
    storage.migrate().await.context("failed to run migrations")?;

    // Build pipeline
    let dedupe = hookbox::dedupe::LayeredDedupe::new(
        hookbox::dedupe::InMemoryRecentDedupe::new(config.dedupe.lru_capacity),
        hookbox_postgres::StorageDedupe::new(pool.clone()),
    );

    let (emitter, _rx) = hookbox::emitter::ChannelEmitter::new(1024);

    let mut builder = hookbox::HookboxPipeline::builder()
        .storage(storage)
        .dedupe(dedupe)
        .emitter(emitter);

    // Register provider verifiers from config
    for (name, provider_config) in &config.providers {
        match provider_config.r#type.as_str() {
            "stripe" => {
                let mut verifier = hookbox_providers::StripeVerifier::new(provider_config.secret.clone());
                if let Some(tolerance) = provider_config.tolerance_seconds {
                    verifier = verifier.with_tolerance(std::time::Duration::from_secs(tolerance));
                }
                builder = builder.verifier(verifier);
            }
            "hmac-sha256" | _ => {
                let header = provider_config
                    .header
                    .clone()
                    .unwrap_or_else(|| "X-Webhook-Signature".to_owned());
                let verifier = hookbox_providers::GenericHmacVerifier::new(
                    name,
                    provider_config.secret.as_bytes().to_vec(),
                    header,
                );
                builder = builder.verifier(verifier);
            }
        }
    }

    let pipeline = builder.build();

    let state = Arc::new(hookbox_server::AppState {
        pipeline,
        pool: pool.clone(),
    });

    let app = hookbox_server::build_router(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind to {addr}"))?;

    tracing::info!("hookbox server listening on {addr}");

    axum::serve(listener, app)
        .await
        .context("server error")?;

    Ok(())
}
```

- [ ] **Step 4: Add `tokio` dependency to hookbox-cli**

Ensure `crates/hookbox-cli/Cargo.toml` has `tokio` with the `full` feature (already present from workspace).

- [ ] **Step 5: Run lint**

Run: `cargo clippy -p hookbox-cli --all-targets -- -D warnings`

Expected: Clean (may need small fixes).

- [ ] **Step 6: Commit**

```bash
git add crates/hookbox-cli/
git commit -m "feat(hookbox-cli): add serve command with config loading and server startup

hookbox serve --config hookbox.toml starts the full webhook ingestion
server with database connection, migrations, provider verifiers, and
pipeline wiring."
```

---

## Task 13: Full Workspace Lint and Test Pass

**Files:** All crates.

### Steps

- [ ] **Step 1: Run full workspace lint**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Fix any issues.

- [ ] **Step 2: Run full workspace tests**

Run: `cargo test --workspace --all-features`

Expected: All unit tests pass.

- [ ] **Step 3: Run deny check**

Run: `cargo deny check`

Expected: Clean.

- [ ] **Step 4: Run fmt check**

Run: `cargo fmt --all --check`

Expected: Clean.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "chore: fix workspace-wide lint and formatting issues"
```

---

## Task 14: Integration Test with Real Postgres

**Files:**
- Create: `integration-tests/Cargo.toml`
- Create: `integration-tests/tests/ingest_test.rs`
- Modify: Root `Cargo.toml` (add integration-tests to workspace)

### Steps

- [ ] **Step 1: Set up integration test crate**

```toml
# integration-tests/Cargo.toml

[package]
name = "hookbox-integration-tests"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
hookbox.workspace = true
hookbox-postgres.workspace = true
hookbox-providers.workspace = true
hookbox-server.workspace = true
axum.workspace = true
bytes.workspace = true
http.workspace = true
serde_json.workspace = true
sqlx.workspace = true
tokio.workspace = true
tower.workspace = true
uuid.workspace = true
```

Add `"integration-tests"` to workspace members in root `Cargo.toml`.

- [ ] **Step 2: Write ingest integration test**

```rust
// integration-tests/tests/ingest_test.rs

//! Integration tests for the full ingest pipeline with real Postgres.
//!
//! Requires a running Postgres instance. Set DATABASE_URL to connect.
//! Example: DATABASE_URL=postgres://localhost/hookbox_test cargo test -p hookbox-integration-tests

use bytes::Bytes;
use hookbox::dedupe::InMemoryRecentDedupe;
use hookbox::emitter::ChannelEmitter;
use hookbox::state::IngestResult;
use hookbox::HookboxPipeline;
use hookbox_postgres::PostgresStorage;
use http::HeaderMap;
use sqlx::PgPool;

async fn setup_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/hookbox_test".to_owned());
    let pool = PgPool::connect(&url).await.expect("Failed to connect to test database");
    let storage = PostgresStorage::new(pool.clone());
    storage.migrate().await.expect("Failed to run migrations");
    // Clean up from previous runs
    sqlx::query("DELETE FROM webhook_receipts")
        .execute(&pool)
        .await
        .expect("Failed to clean up");
    pool
}

#[tokio::test]
#[ignore] // Requires Postgres — run with: cargo test -p hookbox-integration-tests -- --ignored
async fn ingest_stores_and_deduplicates() {
    let pool = setup_pool().await;
    let storage = PostgresStorage::new(pool.clone());

    let (emitter, mut rx) = ChannelEmitter::new(16);

    let pipeline = HookboxPipeline::builder()
        .storage(storage)
        .dedupe(InMemoryRecentDedupe::new(100))
        .emitter(emitter)
        .build();

    // First ingest — should be accepted
    let body = Bytes::from(r#"{"event":"payment.completed","id":"pay_123"}"#);
    let result = pipeline
        .ingest("test", HeaderMap::new(), body.clone())
        .await
        .unwrap();
    assert!(matches!(result, IngestResult::Accepted { .. }));

    // Should have emitted an event
    let event = rx.try_recv().expect("expected emitted event");
    assert_eq!(event.provider_name, "test");

    // Second ingest with same body — should be duplicate
    let result2 = pipeline
        .ingest("test", HeaderMap::new(), body)
        .await
        .unwrap();
    assert!(matches!(result2, IngestResult::Duplicate { .. }));
}
```

- [ ] **Step 3: Run integration tests (if Postgres is available)**

Run: `DATABASE_URL=postgres://localhost/hookbox_test cargo test -p hookbox-integration-tests -- --ignored`

If Postgres is not available locally, this test is marked `#[ignore]` and CI can run it when a Postgres service is configured.

- [ ] **Step 4: Commit**

```bash
git add integration-tests/ Cargo.toml
git commit -m "test: add integration test for ingest pipeline with real Postgres

Tests full ingest flow including storage and deduplication against
a real Postgres database. Marked #[ignore] for CI without Postgres."
```

---

## Summary

**Recommended execution order** (reordered from task numbering to bring storage online earlier):

| Order | Task | Crate | What it builds |
|-------|------|-------|----------------|
| 1 | Task 1 | hookbox | Domain types, enums, newtypes, payload hash |
| 2 | Task 2 | hookbox | Error types (thiserror) |
| 3 | Task 3 | hookbox | Core traits (SignatureVerifier, Storage, DedupeStrategy, Emitter) |
| 4 | Task 4 | hookbox | InMemoryRecentDedupe, LayeredDedupe |
| 5 | Task 5 | hookbox | CallbackEmitter, ChannelEmitter |
| 6 | Task 9 | hookbox-postgres | SQL migration + module structure |
| 7 | Task 10 | hookbox-postgres | PostgresStorage, StorageDedupe |
| 8 | Task 7 | hookbox-providers | GenericHmacVerifier |
| 9 | Task 8 | hookbox-providers | StripeVerifier |
| 10 | Task 6 | hookbox | HookboxPipeline with builder |
| 11 | Task 11 | hookbox-server | Config, routes (ingest, health, admin), server wiring |
| 12 | Task 12 | hookbox-cli | Clap CLI with serve command |
| 13 | Task 13 | all | Full workspace lint/test pass |
| 14 | Task 14 | integration-tests | Full pipeline test with real Postgres |

**Rationale:** Tasks 1-5 build core types/traits/impls. Tasks 9-10 (Postgres) come next so the authoritative durable store exists before pipeline wiring. Tasks 7-8 (providers) follow. Task 6 (pipeline) wires everything together. Tasks 11-14 build the server, CLI, and integration validation.

## Implementation Notes

- **Async traits:** Use `async-trait` crate for MVP smoothness. Native async fn in traits (Rust 2024) can replace it later.
- **LRU naming:** `InMemoryRecentDedupe` uses a simple bounded map. Rename to `InMemoryRecentDedupe` if the O(n) eviction is misleading. Acceptable for MVP.
- **`exists_by_dedupe_key`:** Not in the core `Storage` trait. `StorageDedupe` in `hookbox-postgres` queries its own backing store directly — that's an implementation detail, not a trait method.
- **`NormalizedEvent.parsed_payload`:** This is the parsed JSON projection, not raw bytes. The authoritative body is `WebhookReceipt.raw_body: Vec<u8>`. Naming matches the final spec.
