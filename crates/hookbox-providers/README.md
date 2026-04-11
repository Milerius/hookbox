# hookbox-providers

Signature verification adapters for hookbox.

Implements the `SignatureVerifier` trait from `hookbox` core for real-world webhook providers. Each adapter verifies incoming webhook signatures and returns a structured `VerificationResult` with status and reason.

## Adapters

```
┌─────────────────────────────────────────────────────────────────┐
│  hookbox-providers                                              │
│                                                                 │
│  StripeVerifier ──────────── Stripe-Signature header            │
│    • HMAC-SHA256 with timestamp tolerance                       │
│    • Configurable tolerance window (default: 5 min)             │
│    • Supports secret rotation (multiple active secrets)         │
│                                                                 │
│  BvnkVerifier ────────────── x-signature header                 │
│    • HMAC-SHA256 over raw body, Base64-encoded signature        │
│    • BVNK new hook service                                      │
│                                                                 │
│  GenericHmacVerifier ─────── Configurable HMAC                  │
│    • SHA-256 or SHA-512                                         │
│    • Configurable: header name, signing key, encoding           │
│    • Covers most HMAC-based webhook providers                   │
│                                                                 │
│  AdyenVerifier ───────────── HmacSignature header               │
│    • HMAC-SHA256, hex-encoded key, Base64-encoded signature     │
│                                                                 │
│  TripleAFiatVerifier ─────── TripleA-Signature header           │
│    • RSA-SHA512 (PKCS#1 v1.5), PEM public key                  │
│    • Base64-encoded signature over raw body                     │
│                                                                 │
│  TripleACryptoVerifier ────── triplea-signature header          │
│    • HMAC-SHA256 over "{timestamp}.{body}"                      │
│    • Timestamped t=<ts>,v1=<hex-sig> format, replay protection  │
│                                                                 │
│  WalapayVerifier ─────────── svix-* headers                     │
│    • Svix HMAC-SHA256, whsec_<base64> secret format             │
│    • Signed payload: "{svix-id}.{svix-timestamp}.{body}"        │
│    • Timestamp tolerance + multi-signature support              │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `stripe` | yes | Stripe webhook signature verification |
| `bvnk` | yes | BVNK new hook service (Base64 HMAC-SHA256, x-signature) |
| `generic-hmac` | yes | Configurable HMAC-SHA256/SHA512 verifier |
| `adyen` | yes | Adyen HMAC-SHA256 (hex key, Base64 sig, HmacSignature header) |
| `triplea` | yes | Triple-A fiat (RSA-SHA512) and crypto (HMAC-SHA256) verifiers |
| `walapay` | yes | Walapay/Svix HMAC-SHA256 (svix-* headers) |

## Security

All signature comparisons use **constant-time comparison** via the `subtle` crate to prevent timing attacks. Signing secrets never appear in logs, metrics, or error messages.

## Usage

```rust
use hookbox_providers::{StripeVerifier, GenericHmacVerifier};

// Stripe: verifies Stripe-Signature header with timestamp tolerance
let stripe = StripeVerifier::new("stripe".to_owned(), stripe_webhook_secret)
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
