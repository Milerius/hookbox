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
            Err(_elapsed) => {
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
            Err(_elapsed) => Err(EmitError::Timeout(format!(
                "redis xadd timed out after {}ms",
                self.timeout.as_millis()
            ))),
        }
    }
}

#[cfg(test)]
#[expect(clippy::panic, reason = "panic is acceptable in test assertions")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_returns_downstream_error_for_unparseable_url() {
        // `redis::Client::open` rejects strings that are not valid URLs.
        match RedisEmitter::new("not a url at all", "stream".to_owned(), None, 100).await {
            Err(EmitError::Downstream(_)) => {}
            Err(other) => panic!("expected Downstream, got {other:?}"),
            Ok(_emitter) => panic!("expected Err, got Ok"),
        }
    }

    #[tokio::test]
    async fn new_returns_downstream_error_for_unsupported_scheme() {
        // Non-redis schemes are rejected at client construction.
        match RedisEmitter::new("http://127.0.0.1:6379", "stream".to_owned(), None, 100).await {
            Err(EmitError::Downstream(_)) => {}
            Err(other) => panic!("expected Downstream, got {other:?}"),
            Ok(_emitter) => panic!("expected Err, got Ok"),
        }
    }

    #[tokio::test]
    async fn new_returns_timeout_error_when_endpoint_is_blackholed() {
        // 192.0.2.1 is in TEST-NET-1 (RFC 5737); packets are silently dropped,
        // so the connect attempt always exceeds the (very short) timeout.
        match RedisEmitter::new("redis://192.0.2.1:6379", "stream".to_owned(), None, 50).await {
            Err(EmitError::Timeout(msg)) => {
                assert!(
                    msg.contains("redis connect timed out"),
                    "unexpected message: {msg}"
                );
            }
            Err(other) => panic!("expected Timeout, got {other:?}"),
            Ok(_emitter) => panic!("expected Err, got Ok"),
        }
    }
}
