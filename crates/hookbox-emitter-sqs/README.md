# hookbox-emitter-sqs

AWS SQS emitter adapter for hookbox. Forwards normalized webhook events to an Amazon SQS queue.

Supports both standard and FIFO queues. When FIFO mode is enabled, the emitter sets `MessageGroupId` to the provider name and `MessageDeduplicationId` to the receipt ID, ensuring ordered, exactly-once delivery within each provider group.

## Configuration

In `hookbox.toml`:

```toml
[emitter]
type = "sqs"

[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events"
region = "us-east-1"   # optional, uses default AWS region chain if omitted
fifo = false           # optional, default: false
```

For FIFO queues:

```toml
[emitter]
type = "sqs"

[emitter.sqs]
queue_url = "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events.fifo"
region = "us-east-1"
fifo = true
```

## Usage

The adapter is wired automatically by `hookbox-server` when `emitter.type = "sqs"` is set in the configuration. No application code changes are needed.

For embedded usage:

```rust
use hookbox_emitter_sqs::SqsEmitter;

let emitter = SqsEmitter::new(
    "https://sqs.us-east-1.amazonaws.com/123456789012/hookbox-events".to_owned(),
    Some("us-east-1"),
    false,
).await?;
```

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or [MIT License](../../LICENSE-MIT) at your option.
