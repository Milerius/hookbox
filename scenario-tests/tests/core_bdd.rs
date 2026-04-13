//! Core BDD test binary — in-memory pipeline, no database, no HTTP.
//!
//! Run with: `cargo test -p hookbox-scenarios --test core_bdd`

use cucumber::World as _;

#[tokio::main]
async fn main() {
    let features = concat!(env!("CARGO_MANIFEST_DIR"), "/features/core");
    // `run_and_exit` exits the process with a non-zero status if any step
    // failed. Plain `run` resolves to the Writer regardless of outcome, so
    // step failures can slip through CI as exit code 0.
    hookbox_scenarios::world::IngestWorld::cucumber()
        .run_and_exit(features)
        .await;
}
