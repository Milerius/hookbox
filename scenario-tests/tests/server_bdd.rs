//! Server BDD test binary — real `hookbox-server` + testcontainer Postgres + reqwest.
//!
//! Only compiled when the `bdd-server` feature is enabled.
//! Run with: `cargo test -p hookbox-scenarios --test server_bdd --features bdd-server`

#[cfg(feature = "bdd-server")]
#[tokio::main]
async fn main() {
    use cucumber::World as _;
    let features = concat!(env!("CARGO_MANIFEST_DIR"), "/features/server");
    // See `core_bdd.rs` for why we use `run_and_exit` instead of `run`.
    hookbox_scenarios::world::ServerWorld::cucumber()
        .run_and_exit(features)
        .await;
}

#[cfg(not(feature = "bdd-server"))]
fn main() {}
