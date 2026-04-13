Feature: Derived receipt state from delivery rows

  Scenario: All deliveries emitted -> receipt state is Emitted
    Given the pipeline is configured with emitters "a, b"
    When a valid webhook is ingested
    And all deliveries complete successfully
    Then the receipt processing_state is "emitted"

  Scenario: One delivery dead-lettered -> receipt state is DeadLettered
    Given the pipeline is configured with emitters "ok, broken"
    And emitter "broken" permanently fails
    When a valid webhook is ingested
    And workers run until all retries exhausted
    Then the receipt processing_state is "dead_lettered"

  Scenario: Pending and in_flight deliveries -> receipt state is Stored
    Given the pipeline is configured with emitters "slow"
    And emitter "slow" is paused
    When a valid webhook is ingested
    Then the receipt processing_state is "stored"
