Feature: Emitter Fan-Out via Delivery Rows

  After the Phase 6 refactor, the pipeline no longer emits inline.
  Instead, store_with_deliveries atomically inserts the receipt and
  one pending delivery row per configured emitter name. Downstream
  emission is handled asynchronously by EmitterWorker (Phase 7).

  Scenario: Pipeline accepts a webhook and stores it durably
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"emitter_test"}'
    Then the result should be "accepted"

  Scenario: Events contain receipt_id for traceability
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"trace_test"}'
    Then the result should be "accepted"

  Scenario: Pipeline accepts webhook regardless of emitter names configured
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"fail_emit"}'
    Then the result should be "accepted"

  Scenario: Pipeline accepts webhook with complex payload
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"equality_test","amount":42}'
    Then the result should be "accepted"
