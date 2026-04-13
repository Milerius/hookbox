//! Core BDD test binary — in-memory pipeline, no database, no HTTP.
//!
//! Run with: `cargo test -p hookbox-scenarios --test core_bdd`

#![allow(missing_docs, reason = "test entrypoint")]

use cucumber::World as _;

#[tokio::main]
async fn main() {
    hookbox_scenarios::world::IngestWorld::run("scenario-tests/features/core").await;
}
