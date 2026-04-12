//! Redis Streams emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a Redis stream via `XADD`. The event is
//! serialised as JSON and stored in a single `data` field. Each `XADD` call
//! is wrapped in [`tokio::time::timeout`] so a hung connection cannot block
//! the pipeline indefinitely.

use std::time::Duration;

use async_trait::async_trait;
use redis::aio::MultiplexedConnection;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A Redis-Streams-backed [`Emitter`] that publishes events to a configured stream.
pub struct RedisEmitter {
    conn: MultiplexedConnection,
    stream: String,
    maxlen: Option<u64>,
    timeout: Duration,
}

impl RedisEmitter {
    /// Create a new [`RedisEmitter`].
    ///
    /// Opens the Redis client and establishes a multiplexed async connection
    /// up-front so the first `emit()` call doesn't pay connection latency.
    ///
    /// # Arguments
    ///
    /// * `url` — Redis connection URL (e.g. `"redis://127.0.0.1:6379"`).
    /// * `stream` — Redis stream key to publish events to.
    /// * `maxlen` — optional approximate trim length (`XADD ~ MAXLEN`).
    /// * `timeout_ms` — per-operation timeout for `XADD` in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the client cannot be created or
    /// the initial connection cannot be established.
    pub async fn new(
        url: &str,
        stream: String,
        maxlen: Option<u64>,
        timeout_ms: u64,
    ) -> Result<Self, EmitError> {
        let client = redis::Client::open(url).map_err(|e| EmitError::Downstream(e.to_string()))?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| EmitError::Downstream(e.to_string()))?;
        Ok(Self {
            conn,
            stream,
            maxlen,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

#[async_trait]
impl Emitter for RedisEmitter {
    async fn emit(&self, _event: &NormalizedEvent) -> Result<(), EmitError> {
        // Real XADD implementation lands in Task 5 — this stub exists so the
        // round-trip test in Task 4 has something to fail against.
        let _ = (&self.conn, &self.stream, self.maxlen, self.timeout);
        Err(EmitError::Downstream("not yet implemented".to_owned()))
    }
}
