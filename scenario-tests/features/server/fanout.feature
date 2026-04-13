Feature: Server fan-out to multiple emitters
  Full-stack HTTP ingestion exercised against a real Axum server + Postgres.
  Each accepted webhook must create a delivery row per configured emitter
  and each EmitterWorker must deliver it exactly once.

  Scenario: accepted webhook fans out to every configured emitter
    Given a running hookbox server with two healthy emitters
    When I POST a webhook with id "evt-fanout-1"
    Then the response status is 200
    And emitter "primary" eventually receives 1 event
    And emitter "secondary" eventually receives 1 event
    And 1 delivery row exists for emitter "primary" in state "emitted"
    And 1 delivery row exists for emitter "secondary" in state "emitted"
