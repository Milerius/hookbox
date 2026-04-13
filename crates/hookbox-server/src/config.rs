//! TOML-based configuration structs for the hookbox server.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Errors that can occur during configuration parsing or normalization.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The configuration is structurally valid TOML but semantically invalid
    /// (e.g. conflicting emitter sections, duplicate names, out-of-range values).
    #[error("invalid configuration: {0}")]
    Validation(String),
    /// The TOML source could not be deserialized into [`HookboxConfig`].
    #[error("failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),
}

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
    /// Legacy single emitter backend settings. Deprecated: use `[[emitters]]` instead.
    #[serde(default)]
    pub emitter: Option<EmitterConfig>,
    /// Canonical multi-emitter list. Preferred over the legacy `[emitter]` section.
    #[serde(default)]
    pub emitters: Vec<EmitterEntry>,
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

/// Emitter backend configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmitterConfig {
    /// Which emitter backend to use: `"channel"` (default), `"kafka"`, `"nats"`, `"sqs"`, or `"redis"`.
    #[serde(rename = "type", default = "default_emitter_type")]
    pub emitter_type: String,
    /// Kafka emitter settings (required when `type = "kafka"`).
    pub kafka: Option<KafkaEmitterConfig>,
    /// NATS emitter settings (required when `type = "nats"`).
    pub nats: Option<NatsEmitterConfig>,
    /// SQS emitter settings (required when `type = "sqs"`).
    pub sqs: Option<SqsEmitterConfig>,
    /// Redis Streams emitter settings (required when `type = "redis"`).
    pub redis: Option<RedisEmitterConfig>,
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self {
            emitter_type: default_emitter_type(),
            kafka: None,
            nats: None,
            sqs: None,
            redis: None,
        }
    }
}

fn default_emitter_type() -> String {
    "channel".to_owned()
}

/// Kafka emitter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KafkaEmitterConfig {
    /// Comma-separated list of Kafka broker addresses (e.g. `"localhost:9092"`).
    pub brokers: String,
    /// Kafka topic to produce events to.
    pub topic: String,
    /// Kafka client identifier (default: `"hookbox"`).
    #[serde(default = "default_kafka_client_id")]
    pub client_id: String,
    /// Required acknowledgements: `"all"`, `"1"`, or `"0"` (default: `"all"`).
    #[serde(default = "default_kafka_acks")]
    pub acks: String,
    /// Produce timeout in milliseconds (default: 5000).
    #[serde(default = "default_kafka_timeout")]
    pub timeout_ms: u64,
}

fn default_kafka_client_id() -> String {
    "hookbox".to_owned()
}

fn default_kafka_acks() -> String {
    "all".to_owned()
}

const fn default_kafka_timeout() -> u64 {
    5000
}

/// NATS emitter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsEmitterConfig {
    /// NATS server URL (e.g. `"nats://localhost:4222"`).
    pub url: String,
    /// NATS subject to publish events to.
    pub subject: String,
}

/// SQS emitter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqsEmitterConfig {
    /// Full SQS queue URL.
    pub queue_url: String,
    /// AWS region override (uses default region chain if `None`).
    pub region: Option<String>,
    /// Whether the queue is a FIFO queue (default: `false`).
    #[serde(default)]
    pub fifo: bool,
    /// Optional endpoint URL override for `LocalStack` or SQS-compatible services.
    pub endpoint_url: Option<String>,
}

/// Redis Streams emitter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisEmitterConfig {
    /// Redis connection URL (e.g. `"redis://127.0.0.1:6379"`).
    pub url: String,
    /// Redis stream key to publish events to.
    pub stream: String,
    /// Optional approximate stream trim length (`XADD ~ MAXLEN`).
    pub maxlen: Option<u64>,
    /// Per-operation timeout for `XADD` in milliseconds (default: 5000).
    #[serde(default = "default_redis_timeout_ms")]
    pub timeout_ms: u64,
}

const fn default_redis_timeout_ms() -> u64 {
    5000
}

/// Per-emitter retry policy configuration.
///
/// Each `[[emitters]]` entry can override these defaults to tune backoff
/// behaviour independently of the global `[retry]` worker settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicyConfig {
    /// Maximum number of delivery attempts before promoting to the dead-letter queue.
    pub max_attempts: i32,
    /// Initial backoff delay in seconds before the first retry.
    pub initial_backoff_seconds: u64,
    /// Maximum backoff cap in seconds (exponential growth is clamped here).
    pub max_backoff_seconds: u64,
    /// Exponential backoff multiplier applied after each failure.
    pub backoff_multiplier: f64,
    /// Random jitter fraction in `[0.0, 1.0]` added to each backoff interval.
    pub jitter: f64,
}

impl Default for RetryPolicyConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff_seconds: 30,
            max_backoff_seconds: 3600,
            backoff_multiplier: 2.0,
            jitter: 0.2,
        }
    }
}

impl RetryPolicyConfig {
    /// Convert this TOML-shaped config into the runtime [`hookbox::state::RetryPolicy`]
    /// used by `EmitterWorker` for backoff and dead-letter decisions.
    #[must_use]
    pub fn into_policy(self) -> hookbox::state::RetryPolicy {
        hookbox::state::RetryPolicy {
            max_attempts: self.max_attempts,
            initial_backoff: std::time::Duration::from_secs(self.initial_backoff_seconds),
            max_backoff: std::time::Duration::from_secs(self.max_backoff_seconds),
            backoff_multiplier: self.backoff_multiplier,
            jitter: self.jitter,
        }
    }
}

/// A single named emitter entry in the `[[emitters]]` array.
///
/// This is the canonical, non-deprecated way to configure one or more
/// downstream emitters. Each entry is independently named, typed, and
/// carries its own retry and concurrency policy.
///
/// `Serialize` is required so future config-validation tooling and the
/// `hookbox config validate` CLI (Task 14+) can round-trip the normalized
/// config back to TOML for diff/output. It is intentionally unused at
/// present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmitterEntry {
    /// Unique name identifying this emitter (must match `[a-zA-Z0-9_-]{1,64}`).
    pub name: String,
    /// Backend type: `"channel"`, `"kafka"`, `"nats"`, `"sqs"`, or `"redis"`.
    #[serde(rename = "type")]
    pub emitter_type: String,
    /// How often to poll for pending receipts, in seconds (default: 5).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
    /// Number of concurrent delivery tasks for this emitter (default: 1, must be >= 1).
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Optional exclusive lease duration in seconds (must be > 0 if set).
    #[serde(default)]
    pub lease_duration_seconds: Option<u64>,
    /// Kafka backend config (required when `type = "kafka"`).
    pub kafka: Option<KafkaEmitterConfig>,
    /// NATS backend config (required when `type = "nats"`).
    pub nats: Option<NatsEmitterConfig>,
    /// SQS backend config (required when `type = "sqs"`).
    pub sqs: Option<SqsEmitterConfig>,
    /// Redis backend config (required when `type = "redis"`).
    pub redis: Option<RedisEmitterConfig>,
    /// Per-emitter retry policy (defaults match the global retry worker settings).
    #[serde(default)]
    pub retry: RetryPolicyConfig,
}

fn default_poll_interval() -> u64 {
    5
}

fn default_concurrency() -> usize {
    1
}

/// Produces an intentionally invalid entry (empty `name` and `emitter_type`)
/// for use with `..Default::default()` in tests. Validation rejects it.
/// Do not use in production code.
impl Default for EmitterEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            emitter_type: String::new(),
            poll_interval_seconds: default_poll_interval(),
            concurrency: default_concurrency(),
            lease_duration_seconds: None,
            kafka: None,
            nats: None,
            sqs: None,
            redis: None,
            retry: RetryPolicyConfig::default(),
        }
    }
}

impl EmitterEntry {
    /// Convert the legacy `[emitter]` section into a named `EmitterEntry`.
    ///
    /// The synthesised entry is named `"default"` so that downstream code has
    /// a stable name to reference even when the user has not migrated yet.
    #[must_use]
    pub fn from_legacy(legacy: EmitterConfig) -> Self {
        Self {
            name: "default".to_owned(),
            emitter_type: legacy.emitter_type,
            kafka: legacy.kafka,
            nats: legacy.nats,
            sqs: legacy.sqs,
            redis: legacy.redis,
            ..Default::default()
        }
    }

    /// Project this entry's backend selection into a legacy-shaped
    /// [`EmitterConfig`] that [`crate::emitter_factory::build_emitter`]
    /// accepts.  Used by `hookbox serve` to reuse the existing factory for
    /// each entry in the `[[emitters]]` array without duplicating the
    /// kafka/nats/sqs/redis/channel dispatch logic.
    #[must_use]
    pub fn to_emitter_config(&self) -> EmitterConfig {
        EmitterConfig {
            emitter_type: self.emitter_type.clone(),
            kafka: self.kafka.clone(),
            nats: self.nats.clone(),
            sqs: self.sqs.clone(),
            redis: self.redis.clone(),
        }
    }
}

/// Returns `true` if `name` is a valid emitter identifier (`[a-zA-Z0-9_-]{1,64}`).
fn is_valid_emitter_name(name: &str) -> bool {
    let len = name.chars().count();
    if !(1..=64).contains(&len) {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parse a TOML string and normalize the resulting config.
///
/// This is the preferred entry point for production config loading — it
/// combines deserialization with the normalization pass that enforces
/// `[[emitters]]` semantics and converts the legacy `[emitter]` section.
///
/// # Errors
///
/// Returns [`ConfigError::Parse`] if the TOML is malformed, or
/// [`ConfigError::Validation`] if the normalized config is semantically
/// invalid (e.g. conflicting emitter sections, duplicate names).
pub fn parse_and_normalize(toml_str: &str) -> Result<(HookboxConfig, Vec<String>), ConfigError> {
    let config: HookboxConfig = toml::from_str(toml_str)?;
    normalize(config)
}

/// Normalize a parsed [`HookboxConfig`], returning the adjusted config plus
/// any deprecation warnings.
///
/// The normalization rules are:
/// - Both `[emitter]` and `[[emitters]]` present → error.
/// - Neither present → error.
/// - Only `[emitter]` present → convert to a single `[[emitters]]` entry named
///   `"default"` and emit a deprecation warning.
/// - Only `[[emitters]]` present → validate and pass through.
///
/// # Errors
///
/// Returns [`ConfigError::Validation`] if the config violates the above rules
/// or if [`validate_emitter_entries`] finds invalid entries.
pub fn normalize(mut config: HookboxConfig) -> Result<(HookboxConfig, Vec<String>), ConfigError> {
    let mut warnings = Vec::new();
    match (&config.emitter, config.emitters.is_empty()) {
        (Some(_), false) => {
            return Err(ConfigError::Validation(
                "use either `[emitter]` (legacy) or `[[emitters]]` (preferred), not both".into(),
            ));
        }
        (None, true) => {
            return Err(ConfigError::Validation("no emitters configured".into()));
        }
        (Some(legacy), true) => {
            warnings.push("`[emitter]` is deprecated; migrate to `[[emitters]]`".into());
            let entry = EmitterEntry::from_legacy(legacy.clone());
            config.emitters = vec![entry];
            config.emitter = None;
        }
        (None, false) => {
            // [[emitters]] present, no legacy [emitter] section — normal path; nothing to normalize.
        }
    }
    validate_emitter_entries(&config.emitters)?;
    Ok((config, warnings))
}

/// Validate that a slice of [`EmitterEntry`] values satisfies all structural
/// constraints (unique names, valid name format, concurrency >= 1, jitter in
/// range, positive lease duration).
///
/// # Errors
///
/// Returns [`ConfigError::Validation`] describing the first violation found.
pub fn validate_emitter_entries(entries: &[EmitterEntry]) -> Result<(), ConfigError> {
    let mut seen = std::collections::HashSet::new();
    for e in entries {
        if !is_valid_emitter_name(&e.name) {
            return Err(ConfigError::Validation(format!(
                "emitter name {:?} does not match [a-zA-Z0-9_-]{{1,64}}",
                e.name
            )));
        }
        if !seen.insert(e.name.as_str()) {
            return Err(ConfigError::Validation(format!(
                "duplicate emitter name {:?}",
                e.name
            )));
        }
        if e.concurrency == 0 {
            return Err(ConfigError::Validation(format!(
                "emitter {:?}: concurrency must be >= 1",
                e.name
            )));
        }
        if e.retry.jitter < 0.0 || e.retry.jitter > 1.0 {
            return Err(ConfigError::Validation(format!(
                "emitter {:?}: retry.jitter must be in [0.0, 1.0]",
                e.name
            )));
        }
        if e.retry.max_attempts < 1 {
            return Err(ConfigError::Validation(format!(
                "emitter {:?}: retry.max_attempts must be >= 1",
                e.name
            )));
        }
        if e.lease_duration_seconds == Some(0) {
            return Err(ConfigError::Validation(format!(
                "emitter {:?}: lease_duration_seconds must be > 0",
                e.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "expect is acceptable in test code")]
#[expect(clippy::unwrap_used, reason = "unwrap is acceptable in test code")]
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
        assert!(config.emitter.is_none());
        assert!(config.emitters.is_empty());
    }

    #[test]
    fn parse_emitter_default() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("default emitter config should parse");
        assert!(config.emitter.is_none());
    }

    #[test]
    fn parse_emitter_kafka() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "kafka"

[emitter.kafka]
brokers = "localhost:9092"
topic = "hookbox-events"
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("kafka emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        assert_eq!(emitter.emitter_type, "kafka");
        let kafka = emitter.kafka.expect("kafka config should be present");
        assert_eq!(kafka.brokers, "localhost:9092");
        assert_eq!(kafka.topic, "hookbox-events");
        assert_eq!(kafka.client_id, "hookbox");
        assert_eq!(kafka.acks, "all");
        assert_eq!(kafka.timeout_ms, 5000);
    }

    #[test]
    fn parse_emitter_kafka_full() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "kafka"

[emitter.kafka]
brokers = "broker1:9092,broker2:9092"
topic = "my-topic"
client_id = "my-client"
acks = "1"
timeout_ms = 3000
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("full kafka emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        let kafka = emitter.kafka.expect("kafka config should be present");
        assert_eq!(kafka.brokers, "broker1:9092,broker2:9092");
        assert_eq!(kafka.topic, "my-topic");
        assert_eq!(kafka.client_id, "my-client");
        assert_eq!(kafka.acks, "1");
        assert_eq!(kafka.timeout_ms, 3000);
    }

    #[test]
    fn parse_emitter_nats() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "nats"

[emitter.nats]
url = "nats://localhost:4222"
subject = "hookbox.events"
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("nats emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        assert_eq!(emitter.emitter_type, "nats");
        let nats = emitter.nats.expect("nats config should be present");
        assert_eq!(nats.url, "nats://localhost:4222");
        assert_eq!(nats.subject, "hookbox.events");
    }

    #[test]
    fn parse_emitter_sqs() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "sqs"

[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events"
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("sqs emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        assert_eq!(emitter.emitter_type, "sqs");
        let sqs = emitter.sqs.expect("sqs config should be present");
        assert_eq!(
            sqs.queue_url,
            "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events"
        );
        assert!(sqs.region.is_none());
        assert!(!sqs.fifo);
    }

    #[test]
    fn parse_emitter_sqs_full() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "sqs"

[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events.fifo"
region = "us-east-1"
fifo = true
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("full sqs emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        let sqs = emitter.sqs.expect("sqs config should be present");
        assert_eq!(sqs.region.as_deref(), Some("us-east-1"));
        assert!(sqs.fifo);
    }

    #[test]
    fn parse_emitter_redis() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "redis"

[emitter.redis]
url = "redis://127.0.0.1:6379"
stream = "hookbox.events"
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("redis emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        assert_eq!(emitter.emitter_type, "redis");
        let redis = emitter.redis.expect("redis config should be present");
        assert_eq!(redis.url, "redis://127.0.0.1:6379");
        assert_eq!(redis.stream, "hookbox.events");
        assert_eq!(redis.maxlen, None);
        assert_eq!(redis.timeout_ms, 5000); // default
    }

    #[test]
    fn parse_emitter_redis_full() {
        let toml_str = r#"
[database]
url = "postgres://localhost/hookbox"

[emitter]
type = "redis"

[emitter.redis]
url = "redis://10.0.0.5:6379"
stream = "hookbox.events"
maxlen = 100000
timeout_ms = 10000
"#;
        let config: HookboxConfig =
            toml::from_str(toml_str).expect("full redis emitter config should parse");
        let emitter = config.emitter.expect("emitter config should be present");
        let redis = emitter.redis.expect("redis config should be present");
        assert_eq!(redis.url, "redis://10.0.0.5:6379");
        assert_eq!(redis.stream, "hookbox.events");
        assert_eq!(redis.maxlen, Some(100_000));
        assert_eq!(redis.timeout_ms, 10_000);
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

        let walapay = config
            .providers
            .get("my_walapay")
            .expect("walapay provider");
        assert_eq!(walapay.verifier_type, "walapay");
        assert_eq!(walapay.secret.as_deref(), Some("whsec_walapay"));

        let checkout = config
            .providers
            .get("my_checkout")
            .expect("checkout provider");
        assert_eq!(checkout.verifier_type, "checkout");
        assert_eq!(checkout.secret.as_deref(), Some("checkout_secret"));
    }

    // --- Task 13: normalize() and validate_emitter_entries() tests ---

    #[test]
    fn normalize_both_emitter_and_emitters_is_error() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [emitter]
            type = "channel"
            [[emitters]]
            name = "foo"
            type = "channel"
        "#;
        let result = parse_and_normalize(toml);
        assert!(result.is_err(), "both emitter and emitters must error");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("both"),
            "expected error mentioning 'both', got: {msg}"
        );
    }

    #[test]
    fn normalize_neither_emitter_nor_emitters_is_error() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
        "#;
        let result = parse_and_normalize(toml);
        assert!(result.is_err(), "no emitters must error");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no emitters"),
            "expected error mentioning 'no emitters', got: {msg}"
        );
    }

    #[test]
    fn normalize_legacy_emitter_becomes_default_entry() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [emitter]
            type = "channel"
        "#;
        let (config, warnings) = parse_and_normalize(toml).expect("should normalize ok");
        assert_eq!(config.emitters.len(), 1);
        assert_eq!(config.emitters[0].name, "default");
        assert!(warnings.iter().any(|w| w.contains("deprecated")));
    }

    #[test]
    fn normalize_emitters_array_passes_through() {
        let toml = r#"
            [database]
            url = "postgres://localhost/test"
            [[emitters]]
            name = "kafka-billing"
            type = "kafka"
            [emitters.kafka]
            brokers = "localhost:9092"
            topic = "events"
            client_id = "hookbox"
            acks = "all"
            timeout_ms = 5000
        "#;
        let (config, _) = parse_and_normalize(toml).expect("should normalize ok");
        assert_eq!(config.emitters.len(), 1);
        assert_eq!(config.emitters[0].name, "kafka-billing");
    }

    #[test]
    fn validate_duplicate_emitter_names_is_error() {
        let entries = vec![
            EmitterEntry {
                name: "a".into(),
                emitter_type: "channel".into(),
                ..Default::default()
            },
            EmitterEntry {
                name: "a".into(),
                emitter_type: "channel".into(),
                ..Default::default()
            },
        ];
        let result = validate_emitter_entries(&entries);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate"),
            "expected error mentioning 'duplicate', got: {msg}"
        );
    }

    #[test]
    fn validate_name_must_match_regex() {
        let bad_name = "has spaces!";
        let entries = vec![EmitterEntry {
            name: bad_name.into(),
            emitter_type: "channel".into(),
            ..Default::default()
        }];
        assert!(validate_emitter_entries(&entries).is_err());
    }

    #[test]
    fn validate_concurrency_zero_is_error() {
        let entries = vec![EmitterEntry {
            name: "a".into(),
            emitter_type: "channel".into(),
            concurrency: 0,
            ..Default::default()
        }];
        let result = validate_emitter_entries(&entries);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("concurrency"),
            "expected error mentioning 'concurrency', got: {msg}"
        );
    }

    #[test]
    fn validate_jitter_outside_range_is_error() {
        let entries = vec![EmitterEntry {
            name: "a".into(),
            emitter_type: "channel".into(),
            retry: RetryPolicyConfig {
                jitter: 1.5,
                ..Default::default()
            },
            ..Default::default()
        }];
        assert!(validate_emitter_entries(&entries).is_err());
    }
}
