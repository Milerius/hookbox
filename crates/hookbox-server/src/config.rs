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
/// The `type` field selects the verifier: `"stripe"` for Stripe-Signature
/// header verification (with timestamp tolerance), or `"hmac-sha256"` (default)
/// for generic HMAC-SHA256.
///
/// **Important:** Stripe providers MUST set `type = "stripe"` explicitly.
/// The default `"hmac-sha256"` cannot validate Stripe's `t=...,v1=...` format.
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    /// Verification algorithm type: `"stripe"` or `"hmac-sha256"` (default).
    #[serde(rename = "type", default = "default_provider_type")]
    pub verifier_type: String,
    /// Shared secret used for signature verification.
    pub secret: String,
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
        assert_eq!(stripe.secret, "whsec_test123");
        assert_eq!(stripe.header.as_deref(), Some("Stripe-Signature"));
        assert_eq!(stripe.tolerance_seconds, Some(300));

        let github = config.providers.get("github").expect("github provider");
        assert_eq!(github.verifier_type, "hmac-sha256");
        assert_eq!(github.secret, "gh_secret");
        assert!(github.header.is_none());
        assert!(github.tolerance_seconds.is_none());

        assert_eq!(config.dedupe.lru_capacity, 50_000);
        assert_eq!(config.admin.bearer_token.as_deref(), Some("supersecret"));
        assert_eq!(config.retry.interval_seconds, 15);
        assert_eq!(config.retry.max_attempts, 3);
    }
}
