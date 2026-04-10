//! Hookbox — a durable webhook inbox.
//!
//! Core library providing traits, types, and pipeline orchestration
//! for webhook receipt, verification, deduplication, storage, and emission.

pub mod dedupe;
pub mod emitter;
pub mod error;
pub mod hash;
pub mod state;
pub mod traits;
pub mod types;

pub use dedupe::{InMemoryRecentDedupe, LayeredDedupe};
pub use emitter::{CallbackEmitter, ChannelEmitter};
pub use error::*;
pub use hash::compute_payload_hash;
pub use state::*;
pub use traits::*;
pub use types::*;
