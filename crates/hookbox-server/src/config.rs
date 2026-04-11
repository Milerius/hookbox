//! TOML-based configuration structs for the hookbox server.

use std::collections::HashMap;

use serde::Deserialize;

/// Top-level configuration for the hookbox server.
#[derive(Debug, Deserialize)]
pub struct HookboxConfig {
    /// HTTP server settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Database connection settings.
    pub database: DatabaseConfig,
    /// Per-provider webhook verification settings, keyed by provider name.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Advisory deduplication settings.
    #[serde(default)]
    pub dedupe: DedupeConfig,
    /// Admin API settings.
    #[serde(default)]
    pub admin: AdminConfig,
    /// Retry worker settings.
    #[serde(default)]
    pub retry: RetryConfig,
}

/// HTTP server bind and limit settings.
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// Host address to bind to.
    #[serde(default = "default_host")]
    pub host: String,
    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Maximum request body size in bytes.
    #[serde(default = "default_body_limit")]
    pub body_limit: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            body_limit: default_body_limit(),
        }
    }
}

/// Database connection settings.
#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    /// `PostgreSQL` connection URL.
    pub url: String,
    /// Maximum number of connections in the pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

/// Per-provider webhook verification configuration.
///
/// Supported `type` values:
/// - `"stripe"` — Stripe-Signature header with timestamp
/// - `"hmac-sha256"` (default) — Generic HMAC-SHA256 with configurable header
/// - `"adyen"` — Adyen HMAC-SHA256 with hex key and Base64 signature
/// - `"bvnk"` — BVNK HMAC-SHA256 with Base64 signature in x-signature
/// - `"triplea-fiat"` — Triple-A fiat RSA-SHA512 with public key
/// - `"triplea-crypto"` — Triple-A crypto HMAC-SHA256 with timestamp
/// - `"walapay"` — Walapay/Svix HMAC-SHA256 with svix-* headers
/// - `"checkout"` — Checkout.com (alias for hmac-sha256 with Cko-Signature header)
///
/// **Important:** Stripe providers MUST set `type = "stripe"` explicitly.
/// The default `"hmac-sha256"` cannot validate Stripe's `t=...,v1=...` format.
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    /// Verification algorithm type: `"stripe"`, `"hmac-sha256"` (default), or one of the
    /// provider-specific types listed above.
    #[serde(rename = "type", default = "default_provider_type")]
    pub verifier_type: String,
    /// Shared secret for HMAC-based providers. Optional for RSA providers.
    #[serde(default)]
    pub secret: Option<String>,
    /// PEM-encoded public key (for RSA-based providers like Triple-A fiat).
    pub public_key: Option<String>,
    /// HTTP header containing the provider signature.
    pub header: Option<String>,
    /// Maximum age of a signed request in seconds before it is rejected.
    pub tolerance_seconds: Option<u64>,
}

/// Advisory deduplication cache settings.
#[derive(Debug, Deserialize)]
pub struct DedupeConfig {
    /// Maximum number of entries in the in-memory LRU cache.
    #[serde(default = "default_lru_capacity")]
    pub lru_capacity: usize,
}

impl Default for DedupeConfig {
    fn default() -> Self {
        Self {
            lru_capacity: default_lru_capacity(),
        }
    }
}

/// Admin API settings.
#[derive(Debug, Default, Deserialize)]
pub struct AdminConfig {
    /// Optional bearer token required for admin endpoints.
    pub bearer_token: Option<String>,
}

/// Retry worker configuration.
#[derive(Debug, Deserialize)]
pub struct RetryConfig {
    /// Retry interval in seconds (default: 30).
    #[serde(default = "default_retry_interval")]
    pub interval_seconds: u64,
    /// Maximum retry attempts before moving to DLQ (default: 5).
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            interval_seconds: default_retry_interval(),
            max_attempts: default_max_attempts(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_owned()
}

const fn default_port() -> u16 {
    8080
}

const fn default_body_limit() -> usize {
    1_048_576 // 1 MB
}

const fn default_max_connections() -> u32 {
    10
}

const fn default_lru_capacity() -> usize {
    10_000
}

fn default_provider_type() -> String {
    "hmac-sha256".to_owned()
}

const fn default_retry_interval() -> u64 {
    30
}

const fn default_max_attempts() -> i32 {
    5
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"
"#;
        let config: HookboxConfig = toml::from_str(toml_str).expect("minimal config should parse");

        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.body_limit, 1_048_576);
        assert_eq!(config.database.url, "postgres://localhost/hookbox");
        assert_eq!(config.database.max_connections, 10);
        assert!(config.providers.is_empty());
        assert_eq!(config.dedupe.lru_capacity, 10_000);
        assert!(config.admin.bearer_token.is_none());
        assert_eq!(config.retry.interval_seconds, 30);
        assert_eq!(config.retry.max_attempts, 5);
    }

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
[server]
host = "127.0.0.1"
port = 9090
body_limit = 2097152

[database]
url = "postgres://user:pass@db:5432/hookbox"
max_connections = 20

[providers.stripe]
type = "hmac-sha256"
secret = "whsec_test123"
header = "Stripe-Signature"
tolerance_seconds = 300

[providers.github]
secret = "gh_secret"

[dedupe]
lru_capacity = 50000

[admin]
bearer_token = "supersecret"

[retry]
interval_seconds = 15
max_attempts = 3
"#;
        let config: HookboxConfig = toml::from_str(toml_str).expect("full config should parse");

        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 9090);
        assert_eq!(config.server.body_limit, 2_097_152);
        assert_eq!(config.database.url, "postgres://user:pass@db:5432/hookbox");
        assert_eq!(config.database.max_connections, 20);

        let stripe = config.providers.get("stripe").expect("stripe provider");
        assert_eq!(stripe.verifier_type, "hmac-sha256");
        assert_eq!(stripe.secret.as_deref(), Some("whsec_test123"));
        assert_eq!(stripe.header.as_deref(), Some("Stripe-Signature"));
        assert_eq!(stripe.tolerance_seconds, Some(300));

        let github = config.providers.get("github").expect("github provider");
        assert_eq!(github.verifier_type, "hmac-sha256");
        assert_eq!(github.secret.as_deref(), Some("gh_secret"));
        assert!(github.header.is_none());
        assert!(github.tolerance_seconds.is_none());

        assert_eq!(config.dedupe.lru_capacity, 50_000);
        assert_eq!(config.admin.bearer_token.as_deref(), Some("supersecret"));
        assert_eq!(config.retry.interval_seconds, 15);
        assert_eq!(config.retry.max_attempts, 3);
    }

    #[test]
    fn parse_all_new_provider_types() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[providers.my_stripe]
type = "stripe"
secret = "whsec_stripe"

[providers.my_adyen]
type = "adyen"
secret = "0123456789abcdef"

[providers.my_bvnk]
type = "bvnk"
secret = "bvnk_secret"

[providers.my_triplea_fiat]
type = "triplea-fiat"
public_key = "-----BEGIN PUBLIC KEY-----\nMFwwDQYJKoZIhvcNAQEBBQAD\n-----END PUBLIC KEY-----"

[providers.my_triplea_crypto]
type = "triplea-crypto"
secret = "notify_secret"
tolerance_seconds = 300

[providers.my_walapay]
type = "walapay"
secret = "whsec_walapay"

[providers.my_checkout]
type = "checkout"
secret = "checkout_secret"
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("all-provider-types config should parse");

        let adyen = config.providers.get("my_adyen").expect("adyen provider");
        assert_eq!(adyen.verifier_type, "adyen");
        assert_eq!(adyen.secret.as_deref(), Some("0123456789abcdef"));

        let bvnk = config.providers.get("my_bvnk").expect("bvnk provider");
        assert_eq!(bvnk.verifier_type, "bvnk");
        assert_eq!(bvnk.secret.as_deref(), Some("bvnk_secret"));

        let fiat = config
            .providers
            .get("my_triplea_fiat")
            .expect("triplea-fiat provider");
        assert_eq!(fiat.verifier_type, "triplea-fiat");
        assert!(fiat.public_key.is_some());
        assert!(fiat.secret.is_none());

        let crypto = config
            .providers
            .get("my_triplea_crypto")
            .expect("triplea-crypto provider");
        assert_eq!(crypto.verifier_type, "triplea-crypto");
        assert_eq!(crypto.secret.as_deref(), Some("notify_secret"));
        assert_eq!(crypto.tolerance_seconds, Some(300));

        let walapay = config.providers.get("my_walapay").expect("walapay provider");
        assert_eq!(walapay.verifier_type, "walapay");
        assert_eq!(walapay.secret.as_deref(), Some("whsec_walapay"));

        let checkout = config
            .providers
            .get("my_checkout")
            .expect("checkout provider");
        assert_eq!(checkout.verifier_type, "checkout");
        assert_eq!(checkout.secret.as_deref(), Some("checkout_secret"));
    }
}
