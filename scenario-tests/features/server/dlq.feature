Feature: Permanently-failing deliveries land in the dead-letter queue
  After `max_attempts` failures the EmitterWorker must mark the delivery
  row `dead_lettered` and stop retrying.  Other emitters are unaffected.

  Scenario: permanent failure is dead-lettered after max attempts
    Given a running hookbox server where emitter "broken" always fails
    When I POST a webhook with id "evt-dlq-1"
    Then the response status is 200
    And emitter "healthy" eventually receives 1 event
    And 1 delivery row exists for emitter "healthy" in state "emitted"
    And 1 delivery row exists for emitter "broken" in state "dead_lettered"
