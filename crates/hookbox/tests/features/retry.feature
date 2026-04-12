Feature: Retry Worker Behavior

  After the Phase 6 refactor, the pipeline always accepts webhooks
  durably. The retry worker (Phase 7: EmitterWorker) handles
  delivery failures independently of the ingest pipeline.

  Scenario: Receipt is accepted even when no emitters are configured
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"retry_test"}'
    Then the result should be "accepted"

  Scenario: Receipt accepted with emitter names configured
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"emit_fail_test"}'
    Then the result should be "accepted"
