Feature: Server retry drives flaky emitters to success
  When an emitter fails once and then recovers, the per-emitter
  EmitterWorker must retry via the `webhook_deliveries.next_attempt_at`
  schedule and eventually mark the row as emitted.  Unrelated emitters must
  not be affected.

  Scenario: flaky emitter recovers after first failure
    Given a running hookbox server where emitter "flaky" fails once then recovers
    When I POST a webhook with id "evt-retry-1"
    Then the response status is 200
    And emitter "healthy" eventually receives 1 event
    And emitter "flaky" has at least 2 attempts
    And emitter "flaky" eventually receives 1 event
    And 1 delivery row exists for emitter "flaky" in state "emitted"
    And 1 delivery row exists for emitter "healthy" in state "emitted"
