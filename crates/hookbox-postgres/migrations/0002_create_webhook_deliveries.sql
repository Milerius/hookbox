-- Migration 0002: create webhook_deliveries for fan-out delivery tracking.
-- Single transaction: table + indexes + backfill in one shot.
-- Cost: O(n) against existing receipts. For >10M rows, run during a low-
-- traffic window.

-- 1. Create the table.
CREATE TABLE webhook_deliveries (
    delivery_id      UUID PRIMARY KEY,
    receipt_id       UUID NOT NULL REFERENCES webhook_receipts(receipt_id) ON DELETE CASCADE,
    emitter_name     TEXT NOT NULL,
    state            TEXT NOT NULL,
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    last_attempt_at  TIMESTAMPTZ,
    next_attempt_at  TIMESTAMPTZ NOT NULL,
    emitted_at       TIMESTAMPTZ,
    immutable        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 2. Indexes.
-- Hot worker query: pending/failed deliveries ready to dispatch for a given emitter.
CREATE INDEX idx_webhook_deliveries_dispatch
    ON webhook_deliveries (emitter_name, next_attempt_at)
    WHERE state IN ('pending', 'failed') AND immutable = FALSE;

-- DLQ depth + listing.
CREATE INDEX idx_webhook_deliveries_dlq
    ON webhook_deliveries (emitter_name, state)
    WHERE state = 'dead_lettered';

-- Receipt-to-deliveries join.
CREATE INDEX idx_webhook_deliveries_receipt
    ON webhook_deliveries (receipt_id);

-- Receipt+emitter composite for latest-per-emitter projection.
CREATE INDEX idx_webhook_deliveries_receipt_emitter
    ON webhook_deliveries (receipt_id, emitter_name);

-- 3. Backfill: one immutable historical row per existing receipt.
-- Workers skip immutable rows; they exist only for audit history.
INSERT INTO webhook_deliveries
    (delivery_id, receipt_id, emitter_name, state, attempt_count,
     last_error, last_attempt_at, next_attempt_at, emitted_at,
     immutable, created_at)
SELECT
    gen_random_uuid(),
    receipt_id,
    'legacy',
    CASE processing_state
        WHEN 'emitted'            THEN 'emitted'
        WHEN 'processed'          THEN 'emitted'
        WHEN 'replayed'           THEN 'emitted'
        WHEN 'emit_failed'        THEN 'failed'
        WHEN 'dead_lettered'      THEN 'dead_lettered'
        WHEN 'stored'             THEN 'failed'
        ELSE                           'emitted'
    END,
    COALESCE(emit_count, 0),
    last_error,
    processed_at,
    received_at,
    CASE WHEN processing_state IN ('emitted', 'processed', 'replayed')
         THEN processed_at END,
    TRUE,
    received_at
FROM webhook_receipts;
