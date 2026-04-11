//! Kani proofs and Bolero property tests for hookbox.
//!
//! This crate is not published — it exists only for verification.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dedupe_props;
mod hash_props;
mod kani_proofs;
mod metrics_props;
mod provider_props;
mod retry_props;
mod state_props;
