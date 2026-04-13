# hookbox-emitter-sqs

AWS SQS emitter adapter for hookbox. Forwards normalized webhook events to an Amazon SQS queue.

Supports both standard and FIFO queues. When FIFO mode is enabled, the emitter sets `MessageGroupId` to the provider name and `MessageDeduplicationId` to the receipt ID, ensuring ordered, exactly-once delivery within each provider group.

## Configuration

In `hookbox.toml`:

```toml
[[emitters]]
name = "sqs"                 # used as the `emitter` label on metrics and /readyz
type = "sqs"
poll_interval_seconds = 1
concurrency = 16

[emitters.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events"
region = "us-east-1"         # optional, uses default AWS region chain if omitted
fifo = false                 # optional, default: false
endpoint_url = "..."         # optional, override for LocalStack or SQS-compatible services

[emitters.retry]
max_attempts = 5
initial_backoff_seconds = 5
max_backoff_seconds = 300
backoff_multiplier = 2.0
jitter = 0.1
```

For FIFO queues:

```toml
[[emitters]]
name = "sqs-fifo"
type = "sqs"
concurrency = 1              # FIFO is ordered — keep concurrency low

[emitters.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events.fifo"
region = "us-east-1"
fifo = true
```

Multiple `[[emitters]]` blocks with `type = "sqs"` are allowed — each runs an independent worker with its own `name`, poll interval, concurrency, and retry policy.

## Usage

The adapter is wired automatically by `hookbox-server` for every `[[emitters]]` entry with `type = "sqs"`. No application code changes are needed.

For embedded usage:

```rust
use hookbox_emitter_sqs::SqsEmitter;

let emitter = SqsEmitter::new(
    "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events".to_owned(),
    Some("us-east-1"),
    false,
    None, // endpoint_url override (for LocalStack, use Some("http://localhost:4566"))
).await?;
```

## Local Testing

**Option 1: LocalStack (no AWS account needed)**

```bash
docker run -d --name localstack -p 4566:4566 localstack/localstack:latest

# Create a test queue
aws --endpoint-url=http://localhost:4566 sqs create-queue \
  --queue-name hookbox-smoke-test --region us-east-1

# Run smoke test
SQS_QUEUE_URL=http://localhost:4566/000000000000/hookbox-smoke-test \
  AWS_REGION=us-east-1 \
  AWS_ACCESS_KEY_ID=test \
  AWS_SECRET_ACCESS_KEY=test \
  cargo test -p hookbox-integration-tests --test emitter_smoke_test -- --ignored sqs_emitter_smoke
```

**Option 2: Real AWS**

```bash
# Login first
aws login

# Create a test queue
aws sqs create-queue --queue-name hookbox-smoke-test --region eu-west-1

# Run smoke test
SQS_QUEUE_URL=https://sqs.eu-west-1.amazonaws.com/<account-id>/hookbox-smoke-test \
  AWS_REGION=eu-west-1 \
  cargo test -p hookbox-integration-tests --test emitter_smoke_test -- --ignored sqs_emitter_smoke

# Clean up
aws sqs delete-queue --queue-url <queue-url> --region eu-west-1
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
