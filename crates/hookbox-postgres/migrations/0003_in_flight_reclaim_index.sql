-- Migration 0003: partial index supporting the in-flight lease scan.
--
-- `reclaim_expired` scans in_flight rows per emitter, filtered by
-- `last_attempt_at < now() - lease_duration`. `count_in_flight` counts
-- in_flight rows per emitter for health/metrics. Both are hot paths and
-- were previously falling through to `idx_webhook_deliveries_dispatch`,
-- which only indexes `state IN ('pending','failed')` — meaning in-flight
-- queries had no supporting index at all.
--
-- A partial index keyed on (emitter_name, last_attempt_at) restricted to
-- `state = 'in_flight' AND immutable = FALSE` lets both queries hit the
-- index directly. Leaving `immutable` in the WHERE keeps the index lean
-- because backfilled legacy rows (immutable = TRUE) are excluded.
CREATE INDEX idx_webhook_deliveries_in_flight_lease
    ON webhook_deliveries (emitter_name, last_attempt_at)
    WHERE state = 'in_flight' AND immutable = FALSE;
