# hookbox-verify

Verification crate for hookbox. Contains Kani proofs and Bolero property tests. Not published — exists only for correctness verification.

## Verification Tiers

```
┌──────────────────────────────────────────────────┐
│  hookbox-verify                                  │
│                                                  │
│  Bolero Property Tests                           │
│  ────────────────────                            │
│  • Pipeline state machine invariants             │
│  • ProcessingState transition legality            │
│  • Dedupe decision consistency                   │
│  • NormalizedEvent construction correctness       │
│                                                  │
│  Kani Proofs                                     │
│  ───────────                                     │
│  • State machine reachability                    │
│  • No invalid state transitions                  │
│  • Dedupe key uniqueness properties              │
│                                                  │
└──────────────────────────────────────────────────┘
```

## Running

```bash
# Property tests (every PR)
cargo test -p hookbox-verify --all-features

# Kani proofs (nightly, requires kani installed)
cargo kani -p hookbox-verify

# Bolero with specific engine
cargo bolero test -p hookbox-verify
```

## What Gets Verified

| Property | Method | Frequency |
|----------|--------|-----------|
| State transitions are legal | Bolero | Every PR |
| No panics in pipeline paths | Bolero | Every PR |
| Dedupe decisions are consistent | Bolero | Every PR |
| State machine completeness | Kani | Nightly |
| No unreachable states | Kani | Nightly |

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
