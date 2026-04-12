//! Redis Streams emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to a Redis stream via `XADD`. The event is
//! serialised as JSON and stored in a single `data` field. Each `XADD` call
//! is wrapped in [`tokio::time::timeout`] so a hung connection cannot block
//! the pipeline indefinitely.

use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// A Redis-Streams-backed [`Emitter`] that publishes events to a configured stream.
///
/// Holds a multiplexed async connection (cheap to clone) and reuses it for
/// every `emit()` call. The connection is created in [`RedisEmitter::new`]
/// so configuration errors surface at startup, not at the first webhook.
pub struct RedisEmitter {
    conn: MultiplexedConnection,
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
