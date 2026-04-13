Feature: Multi-emitter fan-out

  Scenario: Two emitters both receive one event
    Given the pipeline is configured with emitters "chan-a, chan-b"
    When a valid webhook is ingested
    And all deliveries complete successfully
    Then emitter "chan-a" receives 1 event
    And emitter "chan-b" receives 1 event
    And the receipt has 2 delivery rows

  Scenario: Duplicate ingest does not create new deliveries
    Given the pipeline is configured with emitters "chan-a"
    When a valid webhook is ingested
    And the same webhook is ingested again
    And all deliveries complete successfully
    Then emitter "chan-a" receives 1 event
    And the receipt has 1 delivery row
