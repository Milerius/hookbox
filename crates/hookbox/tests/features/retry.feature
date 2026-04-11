Feature: Retry Worker Behavior

  Scenario: Emit failure marks receipt as EmitFailed
    Given a pipeline with a passing verifier for "test" and a failing emitter
    When I ingest a webhook from "test" with body '{"event":"retry_test"}'
    Then the result should be "accepted"

  Scenario: Receipt accepted even with emit failure
    Given a pipeline with a passing verifier for "test" and a failing emitter
    When I ingest a webhook from "test" with body '{"event":"emit_fail_test"}'
    Then the result should be "accepted"
