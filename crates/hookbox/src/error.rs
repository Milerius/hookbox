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
    /// The default [`crate::traits::Storage::store_with_deliveries`] implementation was
    /// invoked with a non-empty `emitter_names` slice, but this backend did
    /// not override it. Production backends must override the method to
    /// transactionally insert one delivery row per emitter; the default impl
    /// only supports `emitter_names.is_empty()`.
    #[error(
        "fan-out not implemented by this Storage backend; override store_with_deliveries to transactionally insert delivery rows"
    )]
    FanOutNotImplemented,
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
