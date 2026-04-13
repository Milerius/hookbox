//! Server startup helpers.
//!
//! Extracts the pure-logic bootstrap steps — retry validation and provider
//! verifier construction — out of the CLI `serve` handler so they can be
//! unit-tested without a Tokio runtime, database, or HTTP listener.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::time::Duration;

use anyhow::Context as _;

use hookbox::traits::SignatureVerifier;
use hookbox_providers::{
    AdyenVerifier, BvnkVerifier, GenericHmacVerifier, StripeVerifier, TripleACryptoVerifier,
    TripleAFiatVerifier, WalapayVerifier,
};

use crate::config::{ProviderConfig, RetryConfig};

/// Validate the `[retry]` section of the parsed TOML config.
///
/// # Errors
///
/// Returns an error if either `interval_seconds` or `max_attempts` is zero.
pub fn validate_retry_config(retry: &RetryConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        retry.interval_seconds >= 1,
        "retry.interval_seconds must be >= 1"
    );
    anyhow::ensure!(retry.max_attempts >= 1, "retry.max_attempts must be >= 1");
    Ok(())
}

/// Build one [`SignatureVerifier`] per entry in `providers`, preserving the
/// map's iteration order so error messages point at the first offending
/// provider in a deterministic way.
///
/// Returns the list boxed so the caller can register them against a
/// [`HookboxPipelineBuilder`] without having to monomorphise over concrete
/// verifier types at every call site.
///
/// # Errors
///
/// Returns an error on the first provider that is missing a required field
/// (`secret`, `public_key`, …) or has an invalid key material format (e.g.
/// malformed Adyen hex, unparseable Triple-A PEM, bad Walapay whsec).
pub fn build_provider_verifiers<H: BuildHasher>(
    providers: &HashMap<String, ProviderConfig, H>,
) -> anyhow::Result<Vec<Box<dyn SignatureVerifier>>> {
    let mut out: Vec<Box<dyn SignatureVerifier>> = Vec::with_capacity(providers.len());
    for (name, provider) in providers {
        out.push(build_one_verifier(name, provider).with_context(|| format!("provider '{name}'"))?);
    }
    Ok(out)
}

fn non_empty_secret<'a>(name: &str, provider: &'a ProviderConfig) -> anyhow::Result<&'a str> {
    provider
        .secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("provider '{name}' requires a non-empty secret"))
}

#[expect(
    clippy::too_many_lines,
    reason = "one match arm per provider type is clearer than per-arm helpers"
)]
fn build_one_verifier(
    name: &str,
    provider: &ProviderConfig,
) -> anyhow::Result<Box<dyn SignatureVerifier>> {
    match provider.verifier_type.as_str() {
        "stripe" => {
            let secret = non_empty_secret(name, provider)?;
            let mut verifier = StripeVerifier::new(name.to_owned(), secret.to_owned());
            if let Some(tolerance_secs) = provider.tolerance_seconds {
                verifier = verifier.with_tolerance(Duration::from_secs(tolerance_secs));
            }
            Ok(Box::new(verifier))
        }
        "adyen" => {
            let secret = provider
                .secret
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "adyen provider '{name}' requires a non-empty secret (hex-encoded HMAC key)"
                    )
                })?;
            let Some(verifier) = AdyenVerifier::new(name, secret) else {
                anyhow::bail!("invalid hex key for Adyen provider '{name}'");
            };
            Ok(Box::new(verifier))
        }
        "bvnk" => {
            let secret = provider
                .secret
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("bvnk provider '{name}' requires a non-empty secret")
                })?;
            Ok(Box::new(BvnkVerifier::new(
                name,
                secret.as_bytes().to_vec(),
            )))
        }
        "triplea-fiat" => {
            let pem = provider
                .public_key
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "triplea-fiat provider '{name}' requires a non-empty public_key"
                    )
                })?;
            let Some(verifier) = TripleAFiatVerifier::new(name, pem) else {
                anyhow::bail!("invalid PEM public key for Triple-A fiat provider '{name}'");
            };
            Ok(Box::new(verifier))
        }
        "triplea-crypto" => {
            let secret = provider
                .secret
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "triplea-crypto provider '{name}' requires a non-empty secret (notify_secret)"
                    )
                })?;
            let mut verifier = TripleACryptoVerifier::new(name.to_owned(), secret.to_owned());
            if let Some(tolerance) = provider.tolerance_seconds {
                verifier = verifier.with_tolerance(Duration::from_secs(tolerance));
            }
            Ok(Box::new(verifier))
        }
        "walapay" => {
            let secret = provider
                .secret
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "walapay provider '{name}' requires a non-empty secret (whsec_...)"
                    )
                })?;
            let Some(mut verifier) = WalapayVerifier::new(name, secret) else {
                anyhow::bail!(
                    "invalid Svix secret for Walapay provider '{name}' (expected whsec_...)"
                );
            };
            if let Some(tolerance) = provider.tolerance_seconds {
                verifier = verifier.with_tolerance(Duration::from_secs(tolerance));
            }
            Ok(Box::new(verifier))
        }
        "checkout" => {
            let secret = provider
                .secret
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("checkout provider '{name}' requires a non-empty secret")
                })?;
            let header = provider
                .header
                .clone()
                .unwrap_or_else(|| "Cko-Signature".to_owned());
            Ok(Box::new(GenericHmacVerifier::new(
                name,
                secret.as_bytes().to_vec(),
                header,
            )))
        }
        "hmac-sha256" => {
            let secret = non_empty_secret(name, provider)?;
            let header = provider
                .header
                .clone()
                .unwrap_or_else(|| format!("X-{name}-Signature"));
            Ok(Box::new(GenericHmacVerifier::new(
                name,
                secret.as_bytes().to_vec(),
                header,
            )))
        }
        _unknown => {
            // Unknown verifier_type falls back to GenericHmacVerifier. The
            // serve-level tracing::warn! is handled by the caller (serve.rs)
            // so this function stays pure.
            let secret = non_empty_secret(name, provider)?;
            let header = provider
                .header
                .clone()
                .unwrap_or_else(|| format!("X-{name}-Signature"));
            Ok(Box::new(GenericHmacVerifier::new(
                name,
                secret.as_bytes().to_vec(),
                header,
            )))
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
#[expect(clippy::panic, reason = "panic is acceptable in test assertions")]
mod tests {
    use super::*;

    fn provider_with_secret(verifier_type: &str, secret: &str) -> ProviderConfig {
        ProviderConfig {
            verifier_type: verifier_type.to_owned(),
            secret: Some(secret.to_owned()),
            public_key: None,
            header: None,
            tolerance_seconds: None,
        }
    }

    /// `Vec<Box<dyn SignatureVerifier>>` does not implement `Debug`, so
    /// `Result::expect_err` cannot apply. This helper returns the error or
    /// panics if the build unexpectedly succeeded.
    fn expect_build_error(providers: &HashMap<String, ProviderConfig>) -> anyhow::Error {
        match build_provider_verifiers(providers) {
            Ok(_) => panic!("expected build_provider_verifiers to return Err"),
            Err(e) => e,
        }
    }

    // ── validate_retry_config ────────────────────────────────────────────────

    #[test]
    fn validate_retry_config_accepts_positive_values() {
        let retry = RetryConfig {
            interval_seconds: 5,
            max_attempts: 3,
        };
        validate_retry_config(&retry).expect("valid retry config should pass");
    }

    #[test]
    fn validate_retry_config_rejects_zero_interval() {
        let retry = RetryConfig {
            interval_seconds: 0,
            max_attempts: 3,
        };
        let err = validate_retry_config(&retry).expect_err("zero interval should fail");
        assert!(err.to_string().contains("interval_seconds"));
    }

    #[test]
    fn validate_retry_config_rejects_zero_max_attempts() {
        let retry = RetryConfig {
            interval_seconds: 5,
            max_attempts: 0,
        };
        let err = validate_retry_config(&retry).expect_err("zero max_attempts should fail");
        assert!(err.to_string().contains("max_attempts"));
    }

    // ── build_provider_verifiers: happy paths ───────────────────────────────

    #[test]
    fn stripe_verifier_registers_with_tolerance() {
        let mut providers = HashMap::new();
        providers.insert(
            "stripe-test".to_owned(),
            ProviderConfig {
                verifier_type: "stripe".to_owned(),
                secret: Some("whsec_test".to_owned()),
                public_key: None,
                header: None,
                tolerance_seconds: Some(600),
            },
        );
        let verifiers = build_provider_verifiers(&providers).expect("stripe should build");
        assert_eq!(verifiers.len(), 1);
        assert_eq!(verifiers[0].provider_name(), "stripe-test");
    }

    #[test]
    fn adyen_verifier_registers_with_hex_key() {
        let mut providers = HashMap::new();
        // 32-byte key as hex (64 chars).
        providers.insert(
            "adyen-test".to_owned(),
            provider_with_secret(
                "adyen",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
        );
        let verifiers = build_provider_verifiers(&providers).expect("adyen should build");
        assert_eq!(verifiers[0].provider_name(), "adyen-test");
    }

    #[test]
    fn bvnk_verifier_registers() {
        let mut providers = HashMap::new();
        providers.insert(
            "bvnk-test".to_owned(),
            provider_with_secret("bvnk", "shared-secret"),
        );
        let verifiers = build_provider_verifiers(&providers).expect("bvnk should build");
        assert_eq!(verifiers[0].provider_name(), "bvnk-test");
    }

    #[test]
    fn triplea_crypto_verifier_registers_with_tolerance() {
        let mut providers = HashMap::new();
        providers.insert(
            "ta-crypto".to_owned(),
            ProviderConfig {
                verifier_type: "triplea-crypto".to_owned(),
                secret: Some("notify-secret".to_owned()),
                public_key: None,
                header: None,
                tolerance_seconds: Some(300),
            },
        );
        let verifiers = build_provider_verifiers(&providers).expect("triplea-crypto should build");
        assert_eq!(verifiers[0].provider_name(), "ta-crypto");
    }

    #[test]
    fn checkout_verifier_uses_default_header() {
        let mut providers = HashMap::new();
        providers.insert(
            "checkout-test".to_owned(),
            provider_with_secret("checkout", "cko-secret"),
        );
        let verifiers = build_provider_verifiers(&providers).expect("checkout should build");
        assert_eq!(verifiers[0].provider_name(), "checkout-test");
    }

    #[test]
    fn checkout_verifier_honors_override_header() {
        let mut providers = HashMap::new();
        providers.insert(
            "checkout-test".to_owned(),
            ProviderConfig {
                verifier_type: "checkout".to_owned(),
                secret: Some("cko-secret".to_owned()),
                public_key: None,
                header: Some("X-Custom-Sig".to_owned()),
                tolerance_seconds: None,
            },
        );
        let verifiers = build_provider_verifiers(&providers).expect("checkout should build");
        assert_eq!(verifiers[0].provider_name(), "checkout-test");
    }

    #[test]
    fn hmac_sha256_registers_with_default_header() {
        let mut providers = HashMap::new();
        providers.insert(
            "github".to_owned(),
            provider_with_secret("hmac-sha256", "gh-secret"),
        );
        let verifiers = build_provider_verifiers(&providers).expect("hmac-sha256 should build");
        assert_eq!(verifiers[0].provider_name(), "github");
    }

    #[test]
    fn unknown_verifier_type_falls_back_to_generic_hmac() {
        let mut providers = HashMap::new();
        providers.insert(
            "weird".to_owned(),
            provider_with_secret("made-up-kind", "some-secret"),
        );
        let verifiers =
            build_provider_verifiers(&providers).expect("unknown type should fall back");
        assert_eq!(verifiers[0].provider_name(), "weird");
    }

    // ── build_provider_verifiers: error paths ────────────────────────────────

    #[test]
    fn stripe_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "stripe-bad".to_owned(),
            ProviderConfig {
                verifier_type: "stripe".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(
            err.to_string().contains("stripe-bad")
                || err
                    .chain()
                    .any(|e| e.to_string().contains("non-empty secret")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn stripe_empty_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "stripe-empty".to_owned(),
            provider_with_secret("stripe", ""),
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("non-empty")));
    }

    #[test]
    fn adyen_invalid_hex_key_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "adyen-bad".to_owned(),
            provider_with_secret("adyen", "not-hex-at-all"),
        );
        let err = expect_build_error(&providers);
        assert!(
            err.chain()
                .any(|e| e.to_string().contains("invalid hex key"))
        );
    }

    #[test]
    fn adyen_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "adyen-missing".to_owned(),
            ProviderConfig {
                verifier_type: "adyen".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("hex-encoded")));
    }

    #[test]
    fn bvnk_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "bvnk-bad".to_owned(),
            ProviderConfig {
                verifier_type: "bvnk".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("bvnk")));
    }

    #[test]
    fn triplea_fiat_missing_public_key_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "ta-fiat".to_owned(),
            ProviderConfig {
                verifier_type: "triplea-fiat".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("public_key")));
    }

    #[test]
    fn triplea_fiat_invalid_pem_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "ta-fiat".to_owned(),
            ProviderConfig {
                verifier_type: "triplea-fiat".to_owned(),
                secret: None,
                public_key: Some("this is not a PEM".to_owned()),
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("invalid PEM")));
    }

    #[test]
    fn triplea_crypto_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "ta-crypto-bad".to_owned(),
            ProviderConfig {
                verifier_type: "triplea-crypto".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("notify_secret")));
    }

    #[test]
    fn walapay_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "walapay-bad".to_owned(),
            ProviderConfig {
                verifier_type: "walapay".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("whsec_")));
    }

    #[test]
    fn walapay_invalid_secret_format_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "walapay-bad".to_owned(),
            provider_with_secret("walapay", "not-a-whsec"),
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("invalid Svix")));
    }

    #[test]
    fn walapay_valid_secret_with_tolerance_registers() {
        // A valid Svix-style secret is base64 of 16+ bytes; use a realistic
        // 32-byte key encoded as whsec_<base64>.
        use base64::Engine as _;
        let key = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        let whsec = format!("whsec_{key}");
        let mut providers = HashMap::new();
        providers.insert(
            "walapay-ok".to_owned(),
            ProviderConfig {
                verifier_type: "walapay".to_owned(),
                secret: Some(whsec),
                public_key: None,
                header: None,
                tolerance_seconds: Some(120),
            },
        );
        let verifiers = build_provider_verifiers(&providers).expect("valid walapay should build");
        assert_eq!(verifiers[0].provider_name(), "walapay-ok");
    }

    #[test]
    fn checkout_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "co".to_owned(),
            ProviderConfig {
                verifier_type: "checkout".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("checkout")));
    }

    #[test]
    fn hmac_missing_secret_errors() {
        let mut providers = HashMap::new();
        providers.insert(
            "github".to_owned(),
            ProviderConfig {
                verifier_type: "hmac-sha256".to_owned(),
                secret: None,
                public_key: None,
                header: None,
                tolerance_seconds: None,
            },
        );
        let err = expect_build_error(&providers);
        assert!(err.chain().any(|e| e.to_string().contains("non-empty")));
    }

    #[test]
    fn multiple_providers_all_register() {
        let mut providers = HashMap::new();
        providers.insert("s".to_owned(), provider_with_secret("stripe", "whsec_abc"));
        providers.insert("g".to_owned(), provider_with_secret("hmac-sha256", "gh"));
        let verifiers = build_provider_verifiers(&providers).expect("both providers should build");
        assert_eq!(verifiers.len(), 2);
    }

    #[test]
    fn empty_providers_map_returns_empty_vec() {
        let providers: HashMap<String, ProviderConfig> = HashMap::new();
        let verifiers = build_provider_verifiers(&providers).expect("empty map should succeed");
        assert!(verifiers.is_empty());
    }
}
