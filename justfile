# Hookbox — Task Runner
# Run `just --list` to see all available commands.

# Install all development tools
setup:
    cargo install cargo-deny --locked
    cargo install cargo-careful --locked
    cargo install cargo-mutants --locked
    cargo install cargo-llvm-cov --locked
    cargo install cargo-fuzz --locked
    cargo install cargo-nextest --locked
    cargo install just --locked
    @echo "Note: kani requires separate install — see https://model-checking.github.io/kani/"
    @echo "All tools installed."

# Verify all tools are available
check-tools:
    @echo "Checking tools..."
    cargo deny --version
    cargo careful --version
    cargo mutants --version
    cargo llvm-cov --version
    cargo nextest --version
    @echo "All tools available."

# Format all code
fmt:
    cargo fmt --all

# Check formatting
fmt-check:
    cargo fmt --all --check

# Lint all code
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Run all tests
test:
    cargo test --all-features

# Run tests with nextest
test-nextest:
    cargo nextest run --all-features

# Run cargo careful
careful:
    cargo +nightly careful test --all-features

# Run cargo deny
deny:
    cargo deny check

# Build docs
doc:
    cargo doc --no-deps --all-features --open

# Run all CI checks locally
ci: fmt-check lint test deny doc

# Coverage report
coverage:
    cargo llvm-cov --all-features --html
    @echo "Report: target/llvm-cov/html/index.html"

# Coverage with branch coverage (per-crate to avoid llvm-cov SIGSEGV on async-trait crates)
coverage-branch:
    cargo +nightly llvm-cov --branch -p hookbox-server --tests --html
    cargo +nightly llvm-cov --branch -p hookbox-verify --lib --html
    @echo "Branch coverage for compatible crates: target/llvm-cov/html/index.html"
    @echo "Note: hookbox/hookbox-postgres/hookbox-providers crash llvm-cov with --branch due to async-trait macros"

# Run mutation testing
mutants:
    cargo mutants --timeout 60

# Run Kani proofs (requires kani installed)
kani:
    cargo kani -p hookbox-verify
