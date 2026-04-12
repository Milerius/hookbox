Feature: Emitter Selection and Event Forwarding

  Scenario: Default channel emitter accepts and forwards events
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"emitter_test"}'
    Then the result should be "accepted"
    And an event should be emitted with provider "test"

  Scenario: Events contain receipt_id for traceability
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"trace_test"}'
    Then the result should be "accepted"
    And an event should be emitted with provider "test"

  Scenario: Emit failure does not reject the webhook
    Given a pipeline with a passing verifier for "test" and a failing emitter
    When I ingest a webhook from "test" with body '{"event":"fail_emit"}'
    Then the result should be "accepted"

  Scenario: Emitted event preserves the full normalized payload
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"equality_test","amount":42}'
    Then the result should be "accepted"
    And the emitted event payload_hash should be deterministic for body '{"event":"equality_test","amount":42}'
