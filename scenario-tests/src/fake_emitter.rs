//! Fake emitter implementations for BDD scenario tests.
//!
//! Provides [`FakeEmitter`] with configurable [`EmitterBehavior`] to simulate
//! healthy, slow, and failing downstream consumers in BDD scenarios.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::sleep;

use hookbox::error::EmitError;
use hookbox::traits::Emitter;
use hookbox::types::NormalizedEvent;

/// Controls how [`FakeEmitter`] responds to `emit` calls.
#[derive(Debug, Clone)]
pub enum EmitterBehavior {
    /// Always succeeds immediately.
    Healthy,
    /// Fails for the given duration after construction, then succeeds.
    FailFor(Duration),
    /// Fails until the given attempt number (1-based), then succeeds.
    FailUntilAttempt(u32),
    /// Succeeds but introduces an artificial delay.
    Slow(Duration),
    /// Always fails.
    AlwaysFail,
}

/// A fake [`Emitter`] that records received events and applies configurable
/// behavior for testing failure and retry scenarios.
pub struct FakeEmitter {
    /// Logical name of this emitter (e.g. `"kafka"`, `"sqs"`).
    pub name: String,
    /// Controls how the emitter responds to `emit` calls.
    pub behavior: Arc<Mutex<EmitterBehavior>>,
    /// All events successfully received by this emitter.
    pub received: Arc<Mutex<Vec<NormalizedEvent>>>,
    attempt_count: Arc<Mutex<u32>>,
    fail_start: Arc<Mutex<Option<tokio::time::Instant>>>,
}

impl std::fmt::Debug for FakeEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeEmitter")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl FakeEmitter {
    /// Create a new [`FakeEmitter`] with the given name and behavior.
    #[must_use]
    pub fn new(name: impl Into<String>, behavior: EmitterBehavior) -> Self {
        Self {
            name: name.into(),
            behavior: Arc::new(Mutex::new(behavior)),
            received: Arc::new(Mutex::new(Vec::new())),
            attempt_count: Arc::new(Mutex::new(0)),
            fail_start: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the number of events successfully received.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned (only possible in tests after
    /// a panic in another thread).
    #[must_use]
    pub fn received_count(&self) -> usize {
        self.received.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Returns the total number of `emit` attempts recorded (successes and
    /// failures combined).
    #[must_use]
    pub fn attempt_count(&self) -> u32 {
        self.attempt_count.lock().map(|g| *g).unwrap_or(0)
    }

    /// Returns a clone of all events received so far.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn received_events(&self) -> Vec<NormalizedEvent> {
        self.received.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Change the behavior at runtime (useful for "recover after N failures" tests).
    ///
    /// Also resets the `FailFor` start timer so the new behavior's window
    /// begins from this moment rather than inheriting a stale `fail_start`
    /// from a previous `FailFor` behavior.
    ///
    /// # Errors
    ///
    /// Returns an error string if the internal mutex is poisoned.
    pub fn set_behavior(&self, behavior: EmitterBehavior) -> Result<(), String> {
        self.behavior
            .lock()
            .map(|mut g| {
                *g = behavior;
            })
            .map_err(|e| e.to_string())?;
        self.fail_start
            .lock()
            .map(|mut g| {
                *g = None;
            })
            .map_err(|e| e.to_string())
    }
}

#[async_trait]
impl Emitter for FakeEmitter {
    async fn emit(&self, event: &NormalizedEvent) -> Result<(), EmitError> {
        let behavior = {
            let guard = self
                .behavior
                .lock()
                .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
            guard.clone()
        };

        let attempt = {
            let mut guard = self
                .attempt_count
                .lock()
                .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
            *guard += 1;
            *guard
        };

        match behavior {
            EmitterBehavior::Healthy => {
                let mut guard = self
                    .received
                    .lock()
                    .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
                guard.push(event.clone());
                Ok(())
            }
            EmitterBehavior::AlwaysFail => Err(EmitError::Downstream(format!(
                "{}: always-fail emitter",
                self.name
            ))),
            EmitterBehavior::FailUntilAttempt(threshold) => {
                if attempt < threshold {
                    Err(EmitError::Downstream(format!(
                        "{}: failing on attempt {attempt}, threshold {threshold}",
                        self.name
                    )))
                } else {
                    let mut guard = self
                        .received
                        .lock()
                        .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
                    guard.push(event.clone());
                    Ok(())
                }
            }
            EmitterBehavior::Slow(delay) => {
                sleep(delay).await;
                let mut guard = self
                    .received
                    .lock()
                    .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
                guard.push(event.clone());
                Ok(())
            }
            EmitterBehavior::FailFor(duration) => {
                let start = {
                    let mut guard = self
                        .fail_start
                        .lock()
                        .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
                    *guard.get_or_insert_with(tokio::time::Instant::now)
                };

                if start.elapsed() < duration {
                    Err(EmitError::Downstream(format!(
                        "{}: failing for {duration:?} since start",
                        self.name
                    )))
                } else {
                    let mut guard = self
                        .received
                        .lock()
                        .map_err(|e| EmitError::Downstream(format!("mutex poisoned: {e}")))?;
                    guard.push(event.clone());
                    Ok(())
                }
            }
        }
    }
}
