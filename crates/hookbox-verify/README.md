# hookbox-verify

Verification crate for hookbox. Contains Kani proofs and Bolero property tests covering the core state machine, retry/backoff arithmetic, dedupe, emitter fan-out, metrics, and provider verification. Not published — it exists only to lock in correctness invariants.

## Verification Tiers

```
┌────────────────────────────────────────────────────────────┐
│  hookbox-verify                                            │
│                                                            │
│  Bolero Property Tests                                     │
│  ─────────────────────                                     │
│  • state_props       — ProcessingState transition legality │
│  • retry_props       — retry scheduling / attempt counting │
│  • backoff_props     — compute_backoff monotonicity /      │
│                        saturation / jitter bounds          │
│  • dedupe_props      — layered-dedupe decision consistency │
│  • emitter_props     — fan-out delivery-row invariants     │
│  • hash_props        — SHA-256 payload hash stability      │
│  • metrics_props     — label cardinality + emission shape  │
│  • provider_props    — signature-verifier input handling   │
│                                                            │
│  Kani Proofs (kani_proofs.rs)                              │
│  ─────────────────────────────                             │
│  • processing_state_variants_are_distinct                  │
│  • happy_path_transition_sequence_is_valid                 │
│  • stored_is_acceptance_boundary                           │
│  • terminal_states_are_not_delivery_states                 │
│  • retry_next_state_always_valid                           │
│  • reset_always_findable                                   │
│  • proof_backoff_no_overflow                               │
│  • proof_aggregate_state_total                             │
│                                                            │
└────────────────────────────────────────────────────────────┘
```

## Running

```bash
# Property tests (every PR)
cargo test -p hookbox-verify --all-features

# Bolero with a specific engine
cargo bolero test -p hookbox-verify

# Kani proofs (nightly, requires `cargo install --locked kani-verifier && cargo kani setup`)
cargo kani -p hookbox-verify
```

## What Gets Verified

| Area | Mechanism | Frequency |
|------|-----------|-----------|
| `ProcessingState` transitions are legal | `state_props` (Bolero) | Every PR |
| No panics on pipeline paths | `state_props`, `emitter_props` | Every PR |
| `compute_backoff` is monotone, saturates, stays within jitter bounds | `backoff_props` | Every PR |
| Layered dedupe agrees with storage authority | `dedupe_props` | Every PR |
| Fan-out produces one delivery row per configured emitter | `emitter_props` | Every PR |
| SHA-256 payload hash stable under equivalent inputs | `hash_props` | Every PR |
| Provider verifiers are total on malformed input | `provider_props` | Every PR |
| Metric label cardinality does not explode | `metrics_props` | Every PR |
| `ProcessingState` variants are distinct; happy path is reachable | `kani_proofs.rs` | Nightly |
| Stored is the acceptance boundary; terminal states aren't delivery states | `kani_proofs.rs` | Nightly |
| `compute_backoff` never overflows; `retry_next_state` never produces invalid states | `kani_proofs.rs` | Nightly |
| `receipt_aggregate_state` is total over all delivery combinations | `kani_proofs.rs` | Nightly |

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
