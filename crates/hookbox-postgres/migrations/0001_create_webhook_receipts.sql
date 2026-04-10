CREATE TABLE IF NOT EXISTS webhook_receipts (
    receipt_id          UUID PRIMARY KEY,
    provider_name       TEXT NOT NULL,
    provider_event_id   TEXT,
    external_reference  TEXT,
    dedupe_key          TEXT NOT NULL,
    payload_hash        TEXT NOT NULL,
    raw_body            BYTEA NOT NULL,
    parsed_payload      JSONB,
    raw_headers         JSONB NOT NULL,
    normalized_event_type TEXT,
    verification_status TEXT NOT NULL,
    verification_reason TEXT,
    processing_state    TEXT NOT NULL,
    emit_count          INTEGER NOT NULL DEFAULT 0,
    last_error          TEXT,
    received_at         TIMESTAMPTZ NOT NULL,
    processed_at        TIMESTAMPTZ,
    metadata            JSONB NOT NULL DEFAULT '{}'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_webhook_receipts_dedupe_key
    ON webhook_receipts (dedupe_key);
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_provider_event
    ON webhook_receipts (provider_name, provider_event_id);
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_external_ref
    ON webhook_receipts (external_reference) WHERE external_reference IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_state
    ON webhook_receipts (processing_state);
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_received_at
    ON webhook_receipts (received_at);
CREATE INDEX IF NOT EXISTS idx_webhook_receipts_provider_state
    ON webhook_receipts (provider_name, processing_state);
