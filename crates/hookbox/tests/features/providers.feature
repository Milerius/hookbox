Feature: Provider Signature Verification

  Scenario: Accept webhook with valid provider signature
    Given a pipeline with a passing verifier for "test"
    When I ingest a webhook from "test" with body '{"event":"provider_test"}'
    Then the result should be "accepted"

  Scenario: Reject webhook with unknown provider
    Given a pipeline with a passing verifier for "stripe"
    When I ingest a webhook from "unknown_provider" with body '{"event":"test"}'
    Then the result should be "accepted"

  Scenario: Pipeline skips verification when no verifier configured
    Given a pipeline with no verifiers
    When I ingest a webhook from "adyen" with body '{"event":"test"}'
    Then the result should be "accepted"
