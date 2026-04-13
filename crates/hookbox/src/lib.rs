//! Hookbox — a durable webhook inbox.
//!
//! Core library providing traits, types, and pipeline orchestration
//! for webhook receipt, verification, deduplication, storage, and emission.

pub mod dedupe;
pub mod emitter;
pub mod error;
pub mod hash;
pub mod pipeline;
pub mod state;
pub mod traits;
pub mod transitions;
pub mod types;

pub use dedupe::{InMemoryRecentDedupe, LayeredDedupe};
pub use emitter::{CallbackEmitter, ChannelEmitter};
pub use error::*;
pub use hash::compute_payload_hash;
pub use pipeline::{HookboxPipeline, HookboxPipelineBuilder};
pub use state::*;
pub use traits::*;
pub use transitions::{compute_backoff, receipt_aggregate_state, receipt_deliveries_summary};
pub use types::*;
