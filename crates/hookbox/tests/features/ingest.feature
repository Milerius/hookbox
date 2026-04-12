Feature: Webhook Ingest Pipeline

  The hookbox pipeline receives webhooks, verifies signatures,
  deduplicates, and stores durably (with delivery rows).
  Downstream emission is handled asynchronously by EmitterWorker.

  Scenario: Accept a valid webhook
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"payment.completed"}'
    Then the result should be "accepted"

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

  Scenario: Accept webhook with emitter names configured
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"test"}'
    Then the result should be "accepted"
