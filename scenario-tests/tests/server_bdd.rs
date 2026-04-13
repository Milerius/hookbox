//! Server BDD test binary — real `hookbox-server` + testcontainer Postgres + reqwest.
//!
//! Only compiled when the `bdd-server` feature is enabled.
//! Run with: `cargo test -p hookbox-scenarios --test server_bdd --features bdd-server`

#[cfg(feature = "bdd-server")]
#[tokio::main]
async fn main() {
    use cucumber::World as _;
    let features = concat!(env!("CARGO_MANIFEST_DIR"), "/features/server");
    hookbox_scenarios::world::ServerWorld::run(features).await;
}

#[cfg(not(feature = "bdd-server"))]
fn main() {}
