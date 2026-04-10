# hookbox-providers

Signature verification adapters for hookbox.

Implements the `SignatureVerifier` trait from `hookbox` core for real-world webhook providers. Each adapter verifies incoming webhook signatures and returns a structured `VerificationResult` with status and reason.

## Adapters

```
┌─────────────────────────────────────────────────────────────┐
│  hookbox-providers                                          │
│                                                             │
│  StripeVerifier ─────────── Stripe-Signature header         │
│    • HMAC-SHA256 with timestamp tolerance                   │
│    • Configurable tolerance window (default: 5 min)         │
│    • Supports secret rotation (multiple active secrets)     │
│                                                             │
│  BvnkVerifier ───────────── BVNK-style HMAC                │
│    • HMAC-SHA256 over raw body                              │
│    • Configurable header name and encoding                  │
│                                                             │
│  GenericHmacVerifier ────── Configurable HMAC               │
│    • SHA-256 or SHA-512                                     │
│    • Configurable: header name, signing key, encoding       │
│    • Covers most HMAC-based webhook providers               │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `stripe` | yes | Stripe webhook signature verification |
| `bvnk` | yes | BVNK-style HMAC verification |
| `generic-hmac` | yes | Configurable HMAC-SHA256/SHA512 verifier |

## Security

All signature comparisons use **constant-time comparison** via the `subtle` crate to prevent timing attacks. Signing secrets never appear in logs, metrics, or error messages.

## Usage

```rust
use hookbox_providers::{StripeVerifier, GenericHmacVerifier};

// Stripe: verifies Stripe-Signature header with timestamp tolerance
let stripe = StripeVerifier::new(stripe_webhook_secret)
    .with_tolerance(Duration::from_secs(300));

// Generic HMAC: works with most providers
let bvnk = GenericHmacVerifier::builder("bvnk")
    .header("X-Webhook-Signature")
    .algorithm(HmacAlgorithm::Sha256)
    .secret(bvnk_secret)
    .build();

// Register with pipeline
pipeline.builder()
    .verifier(stripe)
    .verifier(bvnk);
```

## Adding a New Provider

Implement the `SignatureVerifier` trait:

```rust
pub trait SignatureVerifier: Send + Sync {
    fn provider_name(&self) -> &str;

    async fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> VerificationResult;
}
```

Return a `VerificationResult` with a machine-readable `reason` string (e.g. `"signature_valid"`, `"timestamp_expired"`, `"missing_signature_header"`).

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
