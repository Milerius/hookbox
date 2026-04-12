//! Redis Streams emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a Redis stream via `XADD`. The event is
//! serialised as JSON and stored in a single `data` field. Each `XADD` call
//! is wrapped in [`tokio::time::timeout`] so a hung connection cannot block
//! the pipeline indefinitely.

use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A Redis-Streams-backed [`Emitter`] that publishes events to a configured stream.
///
/// Holds a [`ConnectionManager`] (cheap to clone) which wraps a multiplexed
/// async connection and re-establishes it in the background on connection
/// errors. The manager is created in [`RedisEmitter::new`] so configuration
/// errors surface at startup, not at the first webhook.
pub struct RedisEmitter {
    conn: ConnectionManager,
    stream: String,
    maxlen: Option<u64>,
    timeout: Duration,
}

impl RedisEmitter {
    /// Create a new [`RedisEmitter`].
    ///
    /// # Arguments
    ///
    /// * `url` — Redis connection URL (e.g. `"redis://127.0.0.1:6379"`).
    /// * `stream` — Redis stream key to publish events to.
    /// * `maxlen` — optional approximate trim length (`XADD ~ MAXLEN`).
    /// * `timeout_ms` — per-operation timeout for `XADD` in milliseconds.
    ///   Also bounds the initial connect at startup so a blackholed Redis
    ///   endpoint cannot hang `hookbox serve` on the OS-level TCP timeout.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the client cannot be created or
    /// the initial connection cannot be established. Returns
    /// [`EmitError::Timeout`] if the initial connect does not complete within
    /// `timeout_ms`.
    pub async fn new(
        url: &str,
        stream: String,
        maxlen: Option<u64>,
        timeout_ms: u64,
    ) -> Result<Self, EmitError> {
        let timeout = Duration::from_millis(timeout_ms);
        let client = redis::Client::open(url).map_err(|e| EmitError::Downstream(e.to_string()))?;
        let config = ConnectionManagerConfig::new().set_connection_timeout(timeout);
        let connect = ConnectionManager::new_with_config(client, config);
        let conn = match tokio::time::timeout(timeout, connect).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => return Err(EmitError::Downstream(e.to_string())),
            Err(_) => {
                return Err(EmitError::Timeout(format!(
                    "redis connect timed out after {}ms",
                    timeout.as_millis()
                )));
            }
        };
        Ok(Self {
            conn,
            stream,
            maxlen,
            timeout,
        })
    }
}

#[async_trait]
impl Emitter for RedisEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let payload =
            serde_json::to_string(event).map_err(|e| EmitError::Downstream(e.to_string()))?;

        let mut conn = self.conn.clone();
        let stream = self.stream.clone();
        let maxlen = self.maxlen;

        let xadd = async move {
            // `*` lets Redis assign the entry ID.
            if let Some(cap) = maxlen {
                conn.xadd_maxlen::<_, _, _, _, ()>(
                    &stream,
                    redis::streams::StreamMaxlen::Approx(
                        usize::try_from(cap).unwrap_or(usize::MAX),
                    ),
                    "*",
                    &[("data", payload.as_str())],
                )
                .await
            } else {
                conn.xadd::<_, _, _, _, ()>(&stream, "*", &[("data", payload.as_str())])
                    .await
            }
        };

        match tokio::time::timeout(self.timeout, xadd).await {
            Ok(Ok(())) => {
                tracing::debug!(
                    receipt_id = %event.receipt_id,
                    stream = %self.stream,
                    "event emitted to redis"
                );
                Ok(())
            }
            Ok(Err(e)) => Err(EmitError::Downstream(format!("redis xadd failed: {e}"))),
            Err(_) => Err(EmitError::Timeout(format!(
                "redis xadd timed out after {}ms",
                self.timeout.as_millis()
            ))),
        }
    }
}
