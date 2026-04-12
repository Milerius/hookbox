//! Health-check endpoints.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
use tokio::time::{Duration, timeout};

use hookbox::traits::{DedupeStrategy, Storage};

use crate::AppState;
use crate::worker::{EmitterHealth, HealthStatus};

/// Per-database health snapshot returned in [`ReadyzResponse`].
#[derive(Debug, Serialize)]
pub(crate) struct DatabaseHealth {
    /// Coarse health classification for the database connection.
    pub(crate) status: HealthStatus,
}

/// Snapshot of a single emitter worker's health, embedded in [`ReadyzResponse`].
#[derive(Debug, Serialize)]
pub(crate) struct EmitterHealthSnapshot {
    /// Coarse health classification.
    pub(crate) status: HealthStatus,
    /// Timestamp of the most recent successful emit.
    pub(crate) last_success_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Timestamp of the most recent failed emit.
    pub(crate) last_failure_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Number of consecutive failures since the last success.
    pub(crate) consecutive_failures: u32,
    /// Current depth of the dead-letter queue for this emitter.
    pub(crate) dlq_depth: u64,
    /// Number of pending deliveries awaiting dispatch for this emitter.
    pub(crate) pending_count: u64,
}

/// Response body returned by `GET /readyz`.
#[derive(Debug, Serialize)]
pub(crate) struct ReadyzResponse {
    /// Aggregate health status across database and all emitters.
    pub(crate) status: HealthStatus,
    /// Database connectivity health.
    pub(crate) database: DatabaseHealth,
    /// Per-emitter health snapshots, keyed by emitter name.
    pub(crate) emitters: BTreeMap<String, EmitterHealthSnapshot>,
}

/// Compute the aggregate [`HealthStatus`] from the database status and
/// a map of emitter health snapshots.
///
/// Rules (in priority order):
/// 1. `Unhealthy` if the database is unhealthy **or** any emitter is unhealthy.
/// 2. `Degraded` if any emitter is degraded.
/// 3. `Healthy` otherwise.
#[must_use]
pub(crate) fn compute_overall_status(
    db: HealthStatus,
    emitters: &BTreeMap<String, EmitterHealthSnapshot>,
) -> HealthStatus {
    if db == HealthStatus::Unhealthy {
        return HealthStatus::Unhealthy;
    }
    let any_unhealthy = emitters
        .values()
        .any(|e| e.status == HealthStatus::Unhealthy);
    if any_unhealthy {
        return HealthStatus::Unhealthy;
    }
    let any_degraded = emitters
        .values()
        .any(|e| e.status == HealthStatus::Degraded);
    if any_degraded {
        return HealthStatus::Degraded;
    }
    HealthStatus::Healthy
}

/// Liveness probe — always returns `200 OK`.
pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — returns `200 OK` when the database is reachable and all
/// emitters are healthy or degraded, `503 Service Unavailable` when the
/// database is unreachable or any emitter is unhealthy.
///
/// The response body is a JSON [`ReadyzResponse`] with per-emitter health
/// snapshots read from each worker's [`ArcSwap<EmitterHealth>`] — no
/// additional database roundtrip is made for emitter health.
pub async fn readyz<S: Storage, D: DedupeStrategy>(
    State(state): State<Arc<AppState<S, D>>>,
) -> impl IntoResponse {
    // ── Database health ────────────────────────────────────────────────────────
    let db_status = match state.pool {
        Some(ref pool) => {
            let ping = sqlx::query("SELECT 1").execute(pool);
            match timeout(Duration::from_secs(2), ping).await {
                Ok(Ok(_)) => HealthStatus::Healthy,
                Ok(Err(_)) | Err(_) => HealthStatus::Unhealthy,
            }
        }
        None => HealthStatus::Unhealthy,
    };

    // ── Emitter snapshots ──────────────────────────────────────────────────────
    let emitters: BTreeMap<String, EmitterHealthSnapshot> = state
        .emitter_health
        .iter()
        .map(|(name, arc_swap)| {
            let h: EmitterHealth = (**arc_swap.load()).clone();
            let snapshot = EmitterHealthSnapshot {
                status: h.status,
                last_success_at: h.last_success_at,
                last_failure_at: h.last_failure_at,
                consecutive_failures: h.consecutive_failures,
                dlq_depth: h.dlq_depth,
                pending_count: h.pending_count,
            };
            (name.clone(), snapshot)
        })
        .collect();

    // ── Aggregate ──────────────────────────────────────────────────────────────
    let overall = compute_overall_status(db_status, &emitters);

    let status_code = match overall {
        HealthStatus::Unhealthy => StatusCode::SERVICE_UNAVAILABLE,
        HealthStatus::Degraded | HealthStatus::Healthy => StatusCode::OK,
    };

    let body = ReadyzResponse {
        status: overall,
        database: DatabaseHealth { status: db_status },
        emitters,
    };

    (status_code, axum::Json(body))
}

/// `GET /metrics` — Prometheus scrape endpoint.
///
/// Returns the current Prometheus metrics in text exposition format if a
/// recorder was installed, or a comment line indicating no recorder is active.
pub async fn metrics<S: Storage, D: DedupeStrategy>(
    State(state): State<Arc<AppState<S, D>>>,
) -> impl IntoResponse {
    match &state.prometheus {
        Some(handle) => handle.render(),
        None => String::from("# no prometheus recorder installed\n"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_snapshot() -> EmitterHealthSnapshot {
        EmitterHealthSnapshot {
            status: HealthStatus::Healthy,
            last_success_at: None,
            last_failure_at: None,
            consecutive_failures: 0,
            dlq_depth: 0,
            pending_count: 0,
        }
    }

    fn degraded_snapshot() -> EmitterHealthSnapshot {
        EmitterHealthSnapshot {
            status: HealthStatus::Degraded,
            last_success_at: None,
            last_failure_at: None,
            consecutive_failures: 1,
            dlq_depth: 0,
            pending_count: 0,
        }
    }

    fn unhealthy_snapshot() -> EmitterHealthSnapshot {
        EmitterHealthSnapshot {
            status: HealthStatus::Unhealthy,
            last_success_at: None,
            last_failure_at: None,
            consecutive_failures: 10,
            dlq_depth: 0,
            pending_count: 0,
        }
    }

    #[test]
    fn all_healthy_yields_healthy() {
        let mut emitters = BTreeMap::new();
        emitters.insert("a".to_owned(), healthy_snapshot());
        emitters.insert("b".to_owned(), healthy_snapshot());

        let result = compute_overall_status(HealthStatus::Healthy, &emitters);
        assert_eq!(result, HealthStatus::Healthy);
    }

    #[test]
    fn one_degraded_yields_degraded() {
        let mut emitters = BTreeMap::new();
        emitters.insert("a".to_owned(), healthy_snapshot());
        emitters.insert("b".to_owned(), degraded_snapshot());

        let result = compute_overall_status(HealthStatus::Healthy, &emitters);
        assert_eq!(result, HealthStatus::Degraded);
    }

    #[test]
    fn one_unhealthy_yields_unhealthy() {
        let mut emitters = BTreeMap::new();
        emitters.insert("a".to_owned(), healthy_snapshot());
        emitters.insert("b".to_owned(), unhealthy_snapshot());

        let result = compute_overall_status(HealthStatus::Healthy, &emitters);
        assert_eq!(result, HealthStatus::Unhealthy);
    }

    #[test]
    fn db_unhealthy_yields_unhealthy_regardless_of_emitters() {
        let mut emitters = BTreeMap::new();
        emitters.insert("a".to_owned(), healthy_snapshot());

        let result = compute_overall_status(HealthStatus::Unhealthy, &emitters);
        assert_eq!(result, HealthStatus::Unhealthy);
    }

    #[test]
    fn degraded_and_unhealthy_yields_unhealthy() {
        let mut emitters = BTreeMap::new();
        emitters.insert("a".to_owned(), degraded_snapshot());
        emitters.insert("b".to_owned(), unhealthy_snapshot());

        let result = compute_overall_status(HealthStatus::Healthy, &emitters);
        assert_eq!(result, HealthStatus::Unhealthy);
    }
}
