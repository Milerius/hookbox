Feature: Webhook Ingest Pipeline

  The hookbox pipeline receives webhooks, verifies signatures,
  deduplicates, stores durably, and emits downstream events.

  Scenario: Accept a valid webhook
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"payment.completed"}'
    Then the result should be "accepted"
    And an event should be emitted with provider "test"

  Scenario: Reject a webhook with invalid signature
    Given a pipeline with a failing verifier for "badprovider"
    When I ingest a webhook from "badprovider" with body '{"event":"test"}'
    Then the result should be "verification_failed"

  Scenario: Deduplicate identical webhooks
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"payment.completed"}'
    And I ingest a webhook from "test" with body '{"event":"payment.completed"}'
    Then the first result should be "accepted"
    And the second result should be "duplicate"

  Scenario: Accept webhook when no verifier is configured
    Given a pipeline with no verifiers
    When I ingest a webhook from "unknown" with body '{"event":"test"}'
    Then the result should be "accepted"

  Scenario: Accept webhook even when emit fails
    Given a pipeline with a passing verifier for "test" and a failing emitter
    When I ingest a webhook from "test" with body '{"event":"test"}'
    Then the result should be "accepted"
