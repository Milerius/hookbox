//! AWS SQS emitter adapter for hookbox.
//!
//! Forwards [`NormalizedEvent`]s to an Amazon SQS queue.  Supports both
//! standard and FIFO queues — when `fifo` is enabled the emitter sets
//! `MessageGroupId` and `MessageDeduplicationId` derived from the event.

use std::time::Duration;

use async_trait::async_trait;
use tokio::time::timeout;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

const SQS_TIMEOUT: Duration = Duration::from_secs(10);

/// An SQS-backed [`Emitter`] that sends events to an Amazon SQS queue.
///
/// When `fifo` is `true`, messages include a `MessageGroupId` set to the
/// provider name and a `MessageDeduplicationId` set to the receipt ID,
/// ensuring ordered, exactly-once delivery within each provider group.
pub struct SqsEmitter {
    client: aws_sdk_sqs::Client,
    queue_url: String,
    fifo: bool,
}

impl SqsEmitter {
    /// Create a new [`SqsEmitter`].
    ///
    /// # Arguments
    ///
    /// * `queue_url` — full SQS queue URL.
    /// * `region` — optional AWS region override; uses the default provider
    ///   chain when `None`.
    /// * `fifo` — set to `true` for FIFO queues.
    /// * `endpoint_url` — optional endpoint URL override for `LocalStack` or
    ///   SQS-compatible services.
    ///
    /// # Errors
    ///
    /// Returns [`EmitError::Downstream`] if the AWS SDK configuration fails.
    pub async fn new(
        queue_url: String,
        region: Option<&str>,
        fifo: bool,
        endpoint_url: Option<&str>,
    ) -> Result<Self, EmitError> {
        let mut config_loader = aws_config::from_env();

        if let Some(r) = region {
            config_loader = config_loader.region(aws_config::Region::new(r.to_owned()));
        }

        if let Some(url) = endpoint_url {
            config_loader = config_loader.endpoint_url(url);
        }

        let config = config_loader.load().await;
        let client = aws_sdk_sqs::Client::new(&config);

        Ok(Self {
            client,
            queue_url,
            fifo,
        })
    }
}

#[async_trait]
impl Emitter for SqsEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let json =
            serde_json::to_string(event).map_err(|e| EmitError::Downstream(e.to_string()))?;

        let mut request = self
            .client
            .send_message()
            .queue_url(&self.queue_url)
            .message_body(&json);

        if self.fifo {
            request = request
                .message_group_id(&event.provider_name)
                .message_deduplication_id(event.receipt_id.to_string());
        }

        match timeout(SQS_TIMEOUT, request.send()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(EmitError::Downstream(format!("sqs send failed: {e:?}"))),
            Err(_) => {
                return Err(EmitError::Timeout(
                    "sqs send timed out after 10s".to_owned(),
                ));
            }
        }

        tracing::debug!(
            receipt_id = %event.receipt_id,
            queue_url = %self.queue_url,
            "event emitted to sqs"
        );

        Ok(())
    }
}
