Feature: Backoff math in dispatch worker

  Scenario: First failure uses initial_backoff
    Given a retry policy with initial_backoff=30s and multiplier=2.0 and jitter=0.0
    When an emitter fails for the first time
    Then the next_attempt_at is approximately 30 seconds in the future

  Scenario: Second failure doubles the backoff
    Given a retry policy with initial_backoff=30s and multiplier=2.0 and jitter=0.0
    When an emitter has failed 2 times
    Then the next_attempt_at is approximately 60 seconds in the future

  Scenario: Backoff is clamped to max_backoff
    Given a retry policy with initial_backoff=60s and multiplier=4.0 and max_backoff=120s
    When an emitter has failed 3 times
    Then the next_attempt_at is approximately 120 seconds in the future
