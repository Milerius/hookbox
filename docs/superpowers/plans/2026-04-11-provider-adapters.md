# Provider Adapters Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 4 new provider signature verifiers (Adyen, BVNK, Triple-A fiat, Walapay) and a Checkout.com config example to the hookbox-providers crate.

**Architecture:** Each verifier is a separate module file with its own feature flag, following the existing `StripeVerifier` and `GenericHmacVerifier` patterns. All implement the `SignatureVerifier` trait via `#[async_trait]`. Tests co-located in each file. New dependencies: `base64` (for Adyen/BVNK/Walapay), `rsa` + `sha2` (for Triple-A RSA-SHA512). Note: Checkout.com is a config-level alias only (wired in `serve.rs` as `GenericHmacVerifier`), not a separate module.

**Tech Stack:** hmac 0.13, sha2 0.11, subtle 2 (constant-time), base64 0.22, rsa (for Triple-A), async-trait, http (HeaderMap).

**Provider research sources:**
- Adyen: https://docs.adyen.com/development-resources/webhooks/secure-webhooks/verify-hmac-signatures
- BVNK: https://docs.bvnk.com/bvnk/references/webhook-validator/
- Checkout.com: https://www.checkout.com/docs/developer-resources/event-notifications/receive-webhooks/configure-your-webhook-server
- Triple-A fiat: https://developers.triple-a.io/docs/fiat-payout-api-doc/519646ce696e6-webhook-notifications
- Walapay (Svix): https://docs.walapay.io/docs/verifying-webhook-signature

**UPDATE:** Triple-A crypto payments use the SAME pattern as Stripe: HMAC-SHA256 with
`t=<timestamp>,v1=<hex-sig>` in a `triplea-signature` header. The WooCommerce plugin
(NaCl crypto_box) was an older API version. Current API v2 is standard HMAC.

**Plan change:** Add a `TripleACryptoVerifier` that reuses the timestamped HMAC pattern
(same as Stripe but with `triplea-signature` header and `notify_secret` as key).
Or better: make a configurable `TimestampedHmacVerifier` that both Stripe and Triple-A
crypto can use. This is Task 4b below.

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `crates/hookbox-providers/src/adyen.rs` | Adyen HMAC-SHA256 (hex key → binary, Base64 signature) |
| `crates/hookbox-providers/src/bvnk.rs` | BVNK HMAC-SHA256 (Base64 signature in `x-signature`) |
| `crates/hookbox-providers/src/triplea.rs` | Triple-A fiat RSA-SHA512 (`TripleA-Signature` header) |
| `crates/hookbox-providers/src/walapay.rs` | Walapay/Svix HMAC-SHA256 (`svix-signature` + timestamp) |

### Modified Files

| File | Changes |
|------|---------|
| `crates/hookbox-providers/src/lib.rs` | Add 4 new module declarations + feature-gated re-exports |
| `crates/hookbox-providers/Cargo.toml` | Add feature flags, `base64` + `rsa` dependencies |
| `Cargo.toml` (workspace root) | Add `rsa` to workspace dependencies |
| `crates/hookbox-providers/README.md` | Add all new providers to docs |
| `crates/hookbox-server/README.md` | Add Checkout.com config example |
| `crates/hookbox-cli/src/commands/serve.rs` | Wire new provider types in config dispatch |
| `crates/hookbox-server/src/config.rs` | Document new provider type values |

---

## Task 1: Add Dependencies and Feature Flags

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/hookbox-providers/Cargo.toml`
- Modify: `crates/hookbox-providers/src/lib.rs`

### Steps

- [ ] **Step 1: Add workspace dependencies**

Add to `[workspace.dependencies]` in root `Cargo.toml`:
```toml
rsa = { version = "0.9", features = ["sha2"] }
```

**Note:** `rsa 0.9` re-exports its own bundled `sha2 0.10` via `rsa::sha2`. The workspace uses `sha2 0.11`, which is a different version and is NOT compatible with `rsa`'s internal digest types. The implementing agent in Task 4 **must** use `rsa::sha2::Sha512` (not the workspace `sha2::Sha512`) to avoid type mismatches.

- [ ] **Step 2: Update hookbox-providers Cargo.toml**

Add new feature flags and dependencies:
```toml
[features]
default = ["stripe", "bvnk", "generic-hmac", "adyen", "triplea", "walapay"]
stripe = []
bvnk = []
generic-hmac = []
adyen = []
triplea = ["dep:rsa"]
walapay = []

[dependencies]
# ... existing deps ...
base64.workspace = true
rsa = { workspace = true, optional = true }
```

Note: `base64` is already a workspace dep (used by hookbox core). `rsa` is optional, only pulled in by the `triplea` feature. There is no `checkout` feature flag — Checkout.com is a config-level alias wired in `serve.rs` (see Task 6), not a separate module.

- [ ] **Step 3: Add module declarations to lib.rs**

```rust
//! Provider signature verification adapters for hookbox.

#[cfg(feature = "generic-hmac")]
pub mod generic_hmac;
#[cfg(feature = "stripe")]
pub mod stripe;
#[cfg(feature = "adyen")]
pub mod adyen;
#[cfg(feature = "bvnk")]
pub mod bvnk;
#[cfg(feature = "triplea")]
pub mod triplea;
#[cfg(feature = "walapay")]
pub mod walapay;

#[cfg(feature = "generic-hmac")]
pub use generic_hmac::GenericHmacVerifier;
#[cfg(feature = "stripe")]
pub use stripe::StripeVerifier;
#[cfg(feature = "adyen")]
pub use adyen::AdyenVerifier;
#[cfg(feature = "bvnk")]
pub use bvnk::BvnkVerifier;
#[cfg(feature = "triplea")]
pub use triplea::TripleAFiatVerifier;
#[cfg(feature = "walapay")]
pub use walapay::WalapayVerifier;
```

- [ ] **Step 4: Create minimal stubs for compilation**

Create each file with a minimal pub struct so the `lib.rs` re-exports compile. The implementing agent should create `adyen.rs`, `bvnk.rs`, `triplea.rs`, `walapay.rs` with the following pattern (replace names accordingly):

```rust
// adyen.rs stub
//! Adyen signature verifier.
/// Adyen HMAC-SHA256 verifier (placeholder).
pub struct AdyenVerifier;
```

```rust
// bvnk.rs stub
//! BVNK signature verifier.
/// BVNK HMAC-SHA256 verifier (placeholder).
pub struct BvnkVerifier;
```

```rust
// triplea.rs stub
//! Triple-A signature verifier.
/// Triple-A fiat RSA-SHA512 verifier (placeholder).
pub struct TripleAFiatVerifier;
```

```rust
// walapay.rs stub
//! Walapay signature verifier.
/// Walapay/Svix HMAC-SHA256 verifier (placeholder).
pub struct WalapayVerifier;
```

These stubs satisfy the re-exports in `lib.rs`. The actual implementation replaces them in later tasks.

- [ ] **Step 5: Run lint**

Run: `cargo clippy -p hookbox-providers --all-targets --all-features -- -D warnings`

- [ ] **Step 6: Commit**

```bash
git commit -m "feat(hookbox-providers): add feature flags and deps for new provider adapters"
```

---

## Task 2: Adyen Verifier

**Files:**
- Create: `crates/hookbox-providers/src/adyen.rs`

### Adyen Signature Specification

- Algorithm: HMAC-SHA256
- Key: hex-encoded string → decode to binary bytes for HMAC
- Signature location: `HmacSignature` HTTP header
- Signature encoding: Base64
- Constant-time comparison required

**Scope:** This verifier supports Adyen's header-based HMAC verification where the signature is sent in the `HmacSignature` HTTP header. The older payload-field approach (`hmacSignature` inside JSON) is not supported.

The implementing agent should:
1. Extract the `HmacSignature` header (return `Failed` if missing)
2. Base64-decode the header value to get the provided signature bytes
3. Compute HMAC-SHA256(hex_decoded_key, raw_body)
4. Constant-time compare the computed MAC with the provided signature bytes

### Steps

- [ ] **Step 1: Implement AdyenVerifier**

```rust
// crates/hookbox-providers/src/adyen.rs

//! Adyen webhook signature verifier.
//!
//! Adyen signs webhooks with HMAC-SHA256. The key is provided as a hex-encoded
//! string and must be decoded to binary. The signature is Base64-encoded and
//! sent in the `HmacSignature` header.
//!
//! Reference: <https://docs.adyen.com/development-resources/webhooks/secure-webhooks/verify-hmac-signatures>

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// Adyen webhook signature verifier.
///
/// Key is hex-encoded, signature is Base64-encoded in the `HmacSignature` header.
pub struct AdyenVerifier {
    provider: String,
    /// HMAC key — decoded from hex to raw bytes.
    key_bytes: Vec<u8>,
}

impl AdyenVerifier {
    /// Create a new [`AdyenVerifier`].
    ///
    /// `hex_key` is the HMAC key from the Adyen Customer Area (hex-encoded string).
    ///
    /// # Errors
    ///
    /// Returns `None` if the hex key is invalid.
    pub fn new(provider: &str, hex_key: &str) -> Option<Self> {
        let key_bytes = hex::decode(hex_key).ok()?;
        Some(Self {
            provider: provider.to_owned(),
            key_bytes,
        })
    }

    fn failed(reason: &str) -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Failed,
            reason: Some(reason.to_owned()),
        }
    }

    fn verified() -> VerificationResult {
        VerificationResult {
            status: VerificationStatus::Verified,
            reason: Some("signature_valid".to_owned()),
        }
    }
}

#[async_trait]
impl SignatureVerifier for AdyenVerifier {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        let Some(sig_header) = headers.get("HmacSignature") else {
            return Self::failed("missing_signature_header");
        };

        let Ok(sig_str) = sig_header.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        use base64::Engine;
        let Ok(provided_sig) = base64::engine::general_purpose::STANDARD.decode(sig_str) else {
            return Self::failed("invalid_base64_signature");
        };

        let Ok(mut mac) = HmacSha256::new_from_slice(&self.key_bytes) else {
            return Self::failed("invalid_key");
        };

        mac.update(body);
        let expected = mac.finalize().into_bytes();

        if expected.ct_eq(&provided_sig).into() {
            Self::verified()
        } else {
            Self::failed("signature_mismatch")
        }
    }
}
```

Add tests: valid signature, invalid signature, missing header, invalid hex key in constructor.

- [ ] **Step 2: Add tests**

The implementing agent should add `#[cfg(test)] mod tests` with at least 4 tests following the existing patterns in `generic_hmac.rs` and `stripe.rs`. The test helper computes a real HMAC-SHA256 with a known hex key, Base64-encodes it, and passes it in the `HmacSignature` header.

- [ ] **Step 3: Run tests and lint**

Run: `cargo test -p hookbox-providers --all-features && cargo clippy -p hookbox-providers --all-targets --all-features -- -D warnings`

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-providers): add AdyenVerifier — HMAC-SHA256 with hex key and Base64 signature"
```

---

## Task 3: BVNK Verifier

**Files:**
- Create: `crates/hookbox-providers/src/bvnk.rs`

### BVNK Signature Specification

- Algorithm: HMAC-SHA256
- Key: raw secret string (UTF-8 bytes)
- Header: `x-signature`
- Signature encoding: Base64
- Signed content: raw JSON body

### Steps

- [ ] **Step 1: Implement BvnkVerifier**

```rust
// crates/hookbox-providers/src/bvnk.rs

//! BVNK webhook signature verifier.
//!
//! BVNK signs webhooks with HMAC-SHA256 over the raw body. The signature is
//! Base64-encoded and sent in the `x-signature` header.
//!
//! Reference: <https://docs.bvnk.com/bvnk/references/webhook-validator/>

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

/// BVNK webhook signature verifier.
///
/// Signs the raw body with HMAC-SHA256, Base64-encodes the result,
/// and sends it in the `x-signature` header.
pub struct BvnkVerifier {
    provider: String,
    secret: Vec<u8>,
}

impl BvnkVerifier {
    /// Create a new [`BvnkVerifier`].
    #[must_use]
    pub fn new(provider: &str, secret: Vec<u8>) -> Self {
        Self {
            provider: provider.to_owned(),
            secret,
        }
    }

    fn failed(reason: &str) -> VerificationResult { /* same pattern */ }
    fn verified() -> VerificationResult { /* same pattern */ }
}

#[async_trait]
impl SignatureVerifier for BvnkVerifier {
    fn provider_name(&self) -> &str { &self.provider }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        let Some(sig_header) = headers.get("x-signature") else {
            return Self::failed("missing_signature_header");
        };
        let Ok(sig_str) = sig_header.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        use base64::Engine;
        let Ok(provided_sig) = base64::engine::general_purpose::STANDARD.decode(sig_str) else {
            return Self::failed("invalid_base64_signature");
        };

        let Ok(mut mac) = HmacSha256::new_from_slice(&self.secret) else {
            return Self::failed("invalid_key");
        };
        mac.update(body);
        let expected = mac.finalize().into_bytes();

        if expected.ct_eq(&provided_sig).into() {
            Self::verified()
        } else {
            Self::failed("signature_mismatch")
        }
    }
}
```

Add tests: valid signature, invalid signature, missing header.

- [ ] **Step 2: Add tests**

Same pattern — compute real HMAC, Base64-encode, pass in `x-signature` header.

- [ ] **Step 3: Run tests and lint**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-providers): add BvnkVerifier — HMAC-SHA256 with Base64 signature"
```

---

## Task 4: Triple-A Fiat Verifier

**Files:**
- Create: `crates/hookbox-providers/src/triplea.rs`

### Triple-A Fiat Signature Specification

- Algorithm: RSA-SHA512
- Header: `TripleA-Signature`
- Signature encoding: Base64
- Key: RSA public key (PEM format, 4096-bit)
- Signed content: `JSON.stringify(req.body)` — the raw JSON body
- No HMAC — asymmetric verification with public key

### Steps

- [ ] **Step 1: Implement TripleAFiatVerifier**

```rust
// crates/hookbox-providers/src/triplea.rs

//! Triple-A fiat payout webhook signature verifier.
//!
//! Triple-A signs fiat payout webhooks with RSA-SHA512 using their public key.
//! The Base64-encoded signature is sent in the `TripleA-Signature` header.
//!
//! Reference: <https://developers.triple-a.io/docs/fiat-payout-api-doc/519646ce696e6-webhook-notifications>
//!
//! Note: Triple-A crypto payments use NaCl `crypto_box` authenticated encryption
//! (not RSA). That requires a separate verifier with `sodiumoxide` — see roadmap.

use async_trait::async_trait;
use http::HeaderMap;
use rsa::pkcs8::DecodePublicKey;
use rsa::sha2::Sha512;
use rsa::signature::Verifier;
use rsa::{RsaPublicKey, pkcs1v15::VerifyingKey};

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

/// Triple-A fiat payout webhook verifier using RSA-SHA512.
pub struct TripleAFiatVerifier {
    provider: String,
    verifying_key: VerifyingKey<Sha512>,
}

impl TripleAFiatVerifier {
    /// Create a new [`TripleAFiatVerifier`] from a PEM-encoded RSA public key.
    ///
    /// # Errors
    ///
    /// Returns `None` if the PEM key cannot be parsed.
    pub fn new(provider: &str, pem_public_key: &str) -> Option<Self> {
        let public_key = RsaPublicKey::from_public_key_pem(pem_public_key).ok()?;
        let verifying_key = VerifyingKey::new(public_key);
        Some(Self {
            provider: provider.to_owned(),
            verifying_key,
        })
    }

    fn failed(reason: &str) -> VerificationResult { /* same pattern */ }
    fn verified() -> VerificationResult { /* same pattern */ }
}

#[async_trait]
impl SignatureVerifier for TripleAFiatVerifier {
    fn provider_name(&self) -> &str { &self.provider }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        let Some(sig_header) = headers.get("TripleA-Signature") else {
            return Self::failed("missing_signature_header");
        };
        let Ok(sig_str) = sig_header.to_str() else {
            return Self::failed("invalid_signature_header_encoding");
        };

        use base64::Engine;
        let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(sig_str) else {
            return Self::failed("invalid_base64_signature");
        };

        let Ok(signature) = rsa::pkcs1v15::Signature::try_from(sig_bytes.as_slice()) else {
            return Self::failed("invalid_rsa_signature_format");
        };

        match self.verifying_key.verify(body, &signature) {
            Ok(()) => Self::verified(),
            Err(_) => Self::failed("signature_mismatch"),
        }
    }
}
```

**IMPORTANT:** The `rsa` crate API may differ from what's shown above. The implementing agent MUST check `rsa 0.9` docs for the exact import paths and types. The key types are:
- `rsa::RsaPublicKey` for the public key
- `rsa::pkcs1v15::VerifyingKey<Sha512>` for PKCS#1 v1.5 verification
- `rsa::signature::Verifier` trait for the `.verify()` method
- `rsa::pkcs8::DecodePublicKey` trait for PEM parsing

Tests should generate an RSA keypair, sign a body, and verify. Use `rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)` for test key generation.

- [ ] **Step 2: Add tests**

Generate a test RSA keypair, sign body with SHA-512 + PKCS1v15, Base64-encode, pass in `TripleA-Signature` header. Test: valid signature, wrong body, missing header.

**Important:** RSA key generation requires an RNG. Add `rand` as a dev-dependency in `crates/hookbox-providers/Cargo.toml`:
```toml
[dev-dependencies]
rand = "0.8"
```
Then use `rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)` in tests.

Also remember: use `rsa::sha2::Sha512` (NOT the workspace `sha2::Sha512`) for both the verifier implementation and test signing, as `rsa 0.9` pins `sha2 0.10` internally.

- [ ] **Step 3: Run tests and lint**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-providers): add TripleAFiatVerifier — RSA-SHA512 with public key"
```

---

## Task 4b: Triple-A Crypto Verifier

**Files:**
- Create: `crates/hookbox-providers/src/triplea_crypto.rs`
- Modify: `crates/hookbox-providers/src/lib.rs` (add module + re-export)

### Triple-A Crypto Signature Specification

- Algorithm: HMAC-SHA256
- Header: `triplea-signature` (lowercase)
- Header format: `t=<unix-timestamp>,v1=<hex-encoded-signature>`
- Secret: `notify_secret` (from payment request)
- Signed content: `{timestamp}.{raw_body}`
- Timestamp tolerance: 300 seconds recommended
- This is the SAME pattern as Stripe but with a different header name

Reference: https://developers.triple-a.io/docs/triplea-api-doc/4c87b81419436-webhook-notifications

Sample data for test validation:
- Body: `{"txs":[{"txid":"33d5d9af65fe63b12e1a4d67f6b05fcf01428764db840463aba621daa65323d3",...}],...}`
- Secret: `Cf9mx4nAvRuy5vwBY2FCtaKr!@#`
- Header: `t=1621246963,v1=a0749b4b490e15701ab2c488037da9367e8421d8cbddf2450071758e2c6c9f7d`

### Steps

- [ ] **Step 1: Implement TripleACryptoVerifier**

Follow the exact same pattern as `StripeVerifier` but with:
- Header name: `triplea-signature` (instead of `Stripe-Signature`)
- Provider name: configurable (like Stripe)
- Secret: raw string (the `notify_secret`)
- Tolerance: 300 seconds default

The implementing agent should read `stripe.rs`, copy the pattern, and change:
1. Header name from `"Stripe-Signature"` to `"triplea-signature"`
2. Module/struct name to `TripleACryptoVerifier`
3. Doc comments to reference Triple-A

- [ ] **Step 2: Add tests using official sample data**

Use the official sample data from Triple-A docs to write a golden test:
```rust
#[tokio::test]
async fn official_sample_data_verifies() {
    let secret = "Cf9mx4nAvRuy5vwBY2FCtaKr!@#";
    let body = r#"{"txs":[{"txid":"33d5d9af65fe63b12e1a4d67f6b05fcf01428764db840463aba621daa65323d3","status":"good",...}]}"#;
    // Use the full raw body from the docs
    let header = "t=1621246963,v1=a0749b4b490e15701ab2c488037da9367e8421d8cbddf2450071758e2c6c9f7d";
    // Skip timestamp check for this test (data is old)
    let verifier = TripleACryptoVerifier::new("triplea".to_owned(), secret.to_owned())
        .with_tolerance(Duration::from_secs(u64::MAX)); // disable for golden test
    // ... verify
}
```

Also add standard tests: valid sig, expired timestamp, wrong secret, missing header.

- [ ] **Step 3: Update lib.rs**

Add to lib.rs:
```rust
#[cfg(feature = "triplea")]
pub mod triplea_crypto;
#[cfg(feature = "triplea")]
pub use triplea_crypto::TripleACryptoVerifier;
```

- [ ] **Step 4: Run tests and lint**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(hookbox-providers): add TripleACryptoVerifier — HMAC-SHA256 timestamped signature"
```

---

## Task 5: Walapay Verifier (Svix)

**Files:**
- Create: `crates/hookbox-providers/src/walapay.rs`

### Walapay/Svix Signature Specification

- Algorithm: HMAC-SHA256
- Headers: `svix-id`, `svix-timestamp`, `svix-signature`
- Signed content: `{svix-id}.{svix-timestamp}.{body}`
- Secret format: `whsec_<base64_key>` — strip `whsec_` prefix, Base64-decode to get key bytes
- Signature format: `v1,<base64_signature>` (can have multiple, space-separated)
- Timestamp tolerance recommended (5 minutes default)

### Steps

- [ ] **Step 1: Implement WalapayVerifier**

```rust
// crates/hookbox-providers/src/walapay.rs

//! Walapay webhook signature verifier (Svix-based).
//!
//! Walapay uses the Svix webhook delivery platform. Signatures are HMAC-SHA256
//! over `{svix-id}.{svix-timestamp}.{body}`, with the secret provided as
//! `whsec_<base64_key>`.
//!
//! Reference: <https://docs.walapay.io/docs/verifying-webhook-signature>

use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use http::HeaderMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use hookbox::state::{VerificationResult, VerificationStatus};
use hookbox::traits::SignatureVerifier;

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_TOLERANCE_SECS: u64 = 300;

/// Walapay (Svix) webhook signature verifier.
pub struct WalapayVerifier {
    provider: String,
    /// HMAC key — decoded from the `whsec_<base64>` secret.
    key_bytes: Vec<u8>,
    tolerance: Duration,
}

impl WalapayVerifier {
    /// Create a new [`WalapayVerifier`].
    ///
    /// `secret` is the full Svix secret string like `whsec_MfKKr9g8GKYq7wJP0B1PLPZtOzLaLaSw`.
    /// The `whsec_` prefix is stripped and the remainder is Base64-decoded.
    ///
    /// Returns `None` if the secret format is invalid.
    pub fn new(provider: &str, secret: &str) -> Option<Self> {
        let b64_part = secret.strip_prefix("whsec_")?;
        use base64::Engine;
        let key_bytes = base64::engine::general_purpose::STANDARD.decode(b64_part).ok()?;
        Some(Self {
            provider: provider.to_owned(),
            key_bytes,
            tolerance: Duration::from_secs(DEFAULT_TOLERANCE_SECS),
        })
    }

    /// Override the timestamp tolerance window.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    fn failed(reason: &str) -> VerificationResult { /* same pattern */ }
    fn verified() -> VerificationResult { /* same pattern */ }
}

#[async_trait]
impl SignatureVerifier for WalapayVerifier {
    fn provider_name(&self) -> &str { &self.provider }

    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> VerificationResult {
        // Extract required Svix headers
        let Some(msg_id) = headers.get("svix-id").and_then(|v| v.to_str().ok()) else {
            return Self::failed("missing_svix_id_header");
        };
        let Some(timestamp_str) = headers.get("svix-timestamp").and_then(|v| v.to_str().ok()) else {
            return Self::failed("missing_svix_timestamp_header");
        };
        let Some(sig_header) = headers.get("svix-signature").and_then(|v| v.to_str().ok()) else {
            return Self::failed("missing_svix_signature_header");
        };

        // Check timestamp tolerance
        let Ok(timestamp) = timestamp_str.parse::<u64>() else {
            return Self::failed("invalid_timestamp");
        };
        let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0);
        if now.abs_diff(timestamp) > self.tolerance.as_secs() {
            return Self::failed("timestamp_expired");
        }

        // Compute expected signature: HMAC-SHA256("{msg_id}.{timestamp}.{body}")
        let Ok(body_str) = std::str::from_utf8(body) else {
            return Self::failed("body_not_utf8");
        };
        let signed_content = format!("{msg_id}.{timestamp_str}.{body_str}");

        let Ok(mut mac) = HmacSha256::new_from_slice(&self.key_bytes) else {
            return Self::failed("invalid_key");
        };
        mac.update(signed_content.as_bytes());
        let expected = mac.finalize().into_bytes();

        use base64::Engine;
        let expected_b64 = base64::engine::general_purpose::STANDARD.encode(&expected);

        // Check against all signatures in the header (space-separated, v1,<sig> format)
        for sig_entry in sig_header.split(' ') {
            let Some(sig_b64) = sig_entry.strip_prefix("v1,") else {
                continue;
            };
            if expected_b64.as_bytes().ct_eq(sig_b64.as_bytes()).into() {
                return Self::verified();
            }
        }

        Self::failed("signature_mismatch")
    }
}
```

Tests: valid signature with Svix headers, missing headers, expired timestamp, wrong secret.

- [ ] **Step 2: Add tests**

The test helper builds Svix headers: `svix-id`, `svix-timestamp` (current unix timestamp), computes HMAC-SHA256 over `{id}.{ts}.{body}` with the decoded key, Base64-encodes as `v1,<sig>`, places in `svix-signature` header.

- [ ] **Step 3: Run tests and lint**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(hookbox-providers): add WalapayVerifier — Svix HMAC-SHA256 with timestamp"
```

---

## Task 6: Wire New Providers in Config and Server

**Files:**
- Modify: `crates/hookbox-cli/src/commands/serve.rs`
- Modify: `crates/hookbox-server/src/config.rs`

### Steps

- [ ] **Step 1: Update config.rs provider type docs**

Update the `ProviderConfig` doc comment to list all supported types:
```
/// Supported `type` values:
/// - `"stripe"` — Stripe-Signature header with timestamp
/// - `"hmac-sha256"` (default) — Generic HMAC-SHA256 with configurable header
/// - `"adyen"` — Adyen HMAC-SHA256 with hex key and Base64 signature
/// - `"bvnk"` — BVNK HMAC-SHA256 with Base64 signature in x-signature
/// - `"triplea-fiat"` — Triple-A fiat RSA-SHA512 with public key
/// - `"triplea-crypto"` — Triple-A crypto HMAC-SHA256 with timestamp (like Stripe)
/// - `"walapay"` — Walapay/Svix HMAC-SHA256 with svix-* headers
/// - `"checkout"` — Checkout.com (alias for hmac-sha256 with Cko-Signature header)
```

Add a new optional field to `ProviderConfig`:
```rust
/// PEM-encoded public key (for RSA-based providers like Triple-A fiat).
pub public_key: Option<String>,
```

**Note on `secret` for RSA providers:** The existing `pub secret: String` field in `ProviderConfig` is unused for RSA-based providers such as `triplea-fiat`. An empty string (`secret = ""`) is acceptable and must be handled gracefully. Config docs and the `triplea-fiat` dispatch branch must document this: "For `triplea-fiat`, `secret` is unused — set it to an empty string or any placeholder value; only `public_key` is required."

- [ ] **Step 2: Update serve.rs provider dispatch**

In `crates/hookbox-cli/src/commands/serve.rs`, find the provider registration loop and add cases:

```rust
match provider.verifier_type.as_str() {
    "stripe" => { /* existing */ }
    "adyen" => {
        let Some(verifier) = hookbox_providers::AdyenVerifier::new(name, &provider.secret) else {
            anyhow::bail!("invalid hex key for Adyen provider '{name}'");
        };
        builder = builder.verifier(verifier);
    }
    "bvnk" => {
        let verifier = hookbox_providers::BvnkVerifier::new(name, provider.secret.as_bytes().to_vec());
        builder = builder.verifier(verifier);
    }
    "triplea-fiat" => {
        let pem = provider.public_key.as_deref()
            .ok_or_else(|| anyhow::anyhow!("triplea-fiat provider '{name}' requires public_key"))?;
        let Some(verifier) = hookbox_providers::TripleAFiatVerifier::new(name, pem) else {
            anyhow::bail!("invalid PEM public key for Triple-A fiat provider '{name}'");
        };
        builder = builder.verifier(verifier);
    }
    "triplea-crypto" => {
        let mut verifier = hookbox_providers::TripleACryptoVerifier::new(name.clone(), provider.secret.clone());
        if let Some(tolerance) = provider.tolerance_seconds {
            verifier = verifier.with_tolerance(Duration::from_secs(tolerance));
        }
        builder = builder.verifier(verifier);
    }
    "walapay" => {
        let Some(verifier) = hookbox_providers::WalapayVerifier::new(name, &provider.secret) else {
            anyhow::bail!("invalid Svix secret for Walapay provider '{name}' (expected whsec_...)");
        };
        builder = builder.verifier(verifier);
    }
    "checkout" => {
        let header = provider.header.clone().unwrap_or_else(|| "Cko-Signature".to_owned());
        let verifier = hookbox_providers::GenericHmacVerifier::new(name, provider.secret.as_bytes().to_vec(), header);
        builder = builder.verifier(verifier);
    }
    "hmac-sha256" | _ => { /* existing generic HMAC fallback */ }
}
```

- [ ] **Step 3: Update config tests**

Add a test in `config.rs` that parses a config with all provider types:
```toml
[providers.adyen_test]
type = "adyen"
secret = "44782DEF547AAA06C910C43D3FCD7F1F..."

[providers.bvnk_test]
type = "bvnk"
secret = "my_bvnk_secret"

[providers.triplea_fiat_test]
type = "triplea-fiat"
secret = "unused_for_rsa"
public_key = "-----BEGIN PUBLIC KEY-----\nMIICIjAN..."

[providers.walapay_test]
type = "walapay"
secret = "whsec_MfKKr9g8GKYq7wJP0B1PLPZtOzLaLaSw"

[providers.checkout_test]
type = "checkout"
secret = "my_checkout_secret"
```

- [ ] **Step 4: Run full lint and test**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features --exclude hookbox-integration-tests`

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(hookbox-server): wire Adyen, BVNK, Triple-A, Walapay, Checkout.com in config"
```

---

## Task 7: Update Documentation

**Files:**
- Modify: `crates/hookbox-providers/README.md`
- Modify: `crates/hookbox-server/README.md`
- Modify: `docs/ROADMAP.md`

### Steps

- [ ] **Step 1: Update providers README**

Add all new adapters to the README table and feature flags section. Add config examples for each.

- [ ] **Step 2: Update server README**

Add config examples for each provider type in the Configuration section.

- [ ] **Step 3: Update ROADMAP**

Mark "Provider adapter pack" as complete. Add note about Triple-A crypto (NaCl) as future work.

- [ ] **Step 4: Commit**

```bash
git commit -m "docs: update READMEs and roadmap with new provider adapters"
```

---

## Task 8: Workspace Validation

### Steps

- [ ] **Step 1: Format, lint, test, deny**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
DATABASE_URL=postgres://localhost/hookbox_test cargo test --workspace --all-features
cargo test -p hookbox --test bdd
cargo deny check
```

- [ ] **Step 2: Commit and push**

```bash
git commit -m "chore: workspace validation"
git push -u origin feat/provider-adapters
```

---

## Summary

| Order | Task | What |
|-------|------|------|
| 1 | Task 1 | Add deps, feature flags, module stubs |
| 2 | Task 2 | AdyenVerifier (HMAC-SHA256, hex key, Base64 sig) |
| 3 | Task 3 | BvnkVerifier (HMAC-SHA256, Base64 sig, x-signature) |
| 4 | Task 4 | TripleAFiatVerifier (RSA-SHA512, public key, TripleA-Signature) |
| 4b | Task 4b | TripleACryptoVerifier (HMAC-SHA256, timestamped, triplea-signature) |
| 5 | Task 5 | WalapayVerifier (Svix HMAC-SHA256, svix-* headers, timestamp) |
| 6 | Task 6 | Wire in config + serve.rs |
| 7 | Task 7 | Documentation |
| 8 | Task 8 | Validation |

**Noted for later:**
- Triple-A Crypto API v1 (WooCommerce plugin) used NaCl `crypto_box` — this is the OLD API. Current API v2 uses standard HMAC-SHA256 with timestamped signatures (now covered by `TripleACryptoVerifier`).
- Checkout.com is just a config alias for GenericHmacVerifier with `Cko-Signature` header
