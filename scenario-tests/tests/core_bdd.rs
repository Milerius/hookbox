//! Core BDD test binary — in-memory pipeline, no database, no HTTP.
//!
//! Run with: `cargo test -p hookbox-scenarios --test core_bdd`

use cucumber::World as _;

#[tokio::main]
async fn main() {
    let features = concat!(env!("CARGO_MANIFEST_DIR"), "/features/core");
    hookbox_scenarios::world::IngestWorld::run(features).await;
}
