//! `PostgreSQL`-backed implementation of the [`Storage`] trait.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use hookbox::error::StorageError;
use hookbox::state::{
    DeliveryId, DeliveryState, ProcessingState, ReceiptId, StoreResult, VerificationStatus,
    WebhookDelivery,
};
use hookbox::traits::Storage;
use hookbox::types::{ReceiptFilter, WebhookReceipt};

/// `PostgreSQL` storage backend for webhook receipts.
///
/// Wraps a [`PgPool`] and implements [`Storage`] against the
/// `webhook_receipts` table created by the bundled migrations.
#[derive(Debug, Clone)]
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Create a new [`PostgresStorage`] from an existing connection pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run all pending migrations against the connected database.
    ///
    /// This embeds the SQL migration files at compile time via
    /// [`sqlx::migrate!`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the migration run fails.
    pub async fn migrate(&self) -> Result<(), StorageError> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))
    }
}

/// Raw database row returned by `SELECT *` on `webhook_receipts`.
struct ReceiptRow {
    receipt_id: Uuid,
    provider_name: String,
    provider_event_id: Option<String>,
    external_reference: Option<String>,
    dedupe_key: String,
    payload_hash: String,
    raw_body: Vec<u8>,
    parsed_payload: Option<serde_json::Value>,
    raw_headers: serde_json::Value,
    normalized_event_type: Option<String>,
    verification_status: String,
    verification_reason: Option<String>,
    processing_state: String,
    emit_count: i32,
    last_error: Option<String>,
    received_at: DateTime<Utc>,
    processed_at: Option<DateTime<Utc>>,
    metadata: serde_json::Value,
}

/// Serialize a serde type to its JSON string value, stripping the surrounding
/// double-quotes that `serde_json::to_string` adds.
///
/// For example, `ProcessingState::Stored` serialises as `"stored"` in JSON,
/// so after stripping quotes the stored TEXT value is `stored`.
fn serialize_enum<T: Serialize>(value: &T) -> Result<String, StorageError> {
    let json =
        serde_json::to_string(value).map_err(|e| StorageError::Serialization(e.to_string()))?;
    Ok(json.trim_matches('"').to_owned())
}

/// Deserialize a `snake_case` TEXT value back to a serde enum by wrapping it in
/// quotes first so `serde_json` treats it as a JSON string.
fn deserialize_enum<T: DeserializeOwned>(s: &str) -> Result<T, StorageError> {
    let quoted = format!("\"{s}\"");
    serde_json::from_str(&quoted).map_err(|e| StorageError::Serialization(e.to_string()))
}

/// Map a raw `sqlx` [`sqlx::postgres::PgRow`] into a [`ReceiptRow`].
fn pg_row_to_receipt_row(r: &sqlx::postgres::PgRow) -> Result<ReceiptRow, StorageError> {
    Ok(ReceiptRow {
        receipt_id: r
            .try_get("receipt_id")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        provider_name: r
            .try_get("provider_name")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        provider_event_id: r
            .try_get("provider_event_id")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        external_reference: r
            .try_get("external_reference")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        dedupe_key: r
            .try_get("dedupe_key")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        payload_hash: r
            .try_get("payload_hash")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        raw_body: r
            .try_get("raw_body")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        parsed_payload: r
            .try_get("parsed_payload")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        raw_headers: r
            .try_get("raw_headers")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        normalized_event_type: r
            .try_get("normalized_event_type")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        verification_status: r
            .try_get("verification_status")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        verification_reason: r
            .try_get("verification_reason")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        processing_state: r
            .try_get("processing_state")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        emit_count: r
            .try_get("emit_count")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        last_error: r
            .try_get("last_error")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        received_at: r
            .try_get("received_at")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        processed_at: r
            .try_get("processed_at")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        metadata: r
            .try_get("metadata")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
    })
}

/// Convert a [`ReceiptRow`] (raw DB row) into a [`WebhookReceipt`].
fn receipt_from_row(row: ReceiptRow) -> Result<WebhookReceipt, StorageError> {
    let verification_status: VerificationStatus = deserialize_enum(&row.verification_status)?;
    let processing_state: ProcessingState = deserialize_enum(&row.processing_state)?;

    Ok(WebhookReceipt {
        receipt_id: ReceiptId(row.receipt_id),
        provider_name: row.provider_name,
        provider_event_id: row.provider_event_id,
        external_reference: row.external_reference,
        dedupe_key: row.dedupe_key,
        payload_hash: row.payload_hash,
        raw_body: row.raw_body,
        parsed_payload: row.parsed_payload,
        raw_headers: row.raw_headers,
        normalized_event_type: row.normalized_event_type,
        verification_status,
        verification_reason: row.verification_reason,
        processing_state,
        emit_count: row.emit_count,
        last_error: row.last_error,
        received_at: row.received_at,
        processed_at: row.processed_at,
        metadata: row.metadata,
    })
}

/// SELECT query used by both [`PostgresStorage::get`] and the query helper.
const SELECT_COLUMNS: &str = r"
    SELECT receipt_id, provider_name, provider_event_id, external_reference,
           dedupe_key, payload_hash, raw_body, parsed_payload, raw_headers,
           normalized_event_type, verification_status, verification_reason,
           processing_state, emit_count, last_error, received_at, processed_at,
           metadata
    FROM webhook_receipts
";

#[async_trait]
impl Storage for PostgresStorage {
    /// Durably store a [`WebhookReceipt`], enforcing uniqueness on `dedupe_key`.
    ///
    /// Uses `INSERT … ON CONFLICT (dedupe_key) DO NOTHING RETURNING receipt_id`.
    /// If the insert is silently dropped (conflict), the existing receipt's ID is
    /// fetched and returned as [`StoreResult::Duplicate`].
    async fn store(&self, receipt: &WebhookReceipt) -> Result<StoreResult, StorageError> {
        let processing_state = serialize_enum(&receipt.processing_state)?;
        let verification_status = serialize_enum(&receipt.verification_status)?;

        let inserted: Option<Uuid> = sqlx::query_scalar(
            r"
            INSERT INTO webhook_receipts (
                receipt_id, provider_name, provider_event_id, external_reference,
                dedupe_key, payload_hash, raw_body, parsed_payload, raw_headers,
                normalized_event_type, verification_status, verification_reason,
                processing_state, emit_count, last_error, received_at, processed_at,
                metadata
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT (dedupe_key) DO NOTHING
            RETURNING receipt_id
            ",
        )
        .bind(receipt.receipt_id.0)
        .bind(&receipt.provider_name)
        .bind(&receipt.provider_event_id)
        .bind(&receipt.external_reference)
        .bind(&receipt.dedupe_key)
        .bind(&receipt.payload_hash)
        .bind(&receipt.raw_body)
        .bind(&receipt.parsed_payload)
        .bind(&receipt.raw_headers)
        .bind(&receipt.normalized_event_type)
        .bind(&verification_status)
        .bind(&receipt.verification_reason)
        .bind(&processing_state)
        .bind(receipt.emit_count)
        .bind(&receipt.last_error)
        .bind(receipt.received_at)
        .bind(receipt.processed_at)
        .bind(&receipt.metadata)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        if inserted.is_some() {
            return Ok(StoreResult::Stored);
        }

        // Conflict: fetch the existing receipt id.
        let existing: Uuid =
            sqlx::query_scalar("SELECT receipt_id FROM webhook_receipts WHERE dedupe_key = $1")
                .bind(&receipt.dedupe_key)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

        Ok(StoreResult::Duplicate {
            existing_id: ReceiptId(existing),
        })
    }

    /// Retrieve a single [`WebhookReceipt`] by its [`Uuid`].
    ///
    /// Returns `Ok(None)` when no receipt with the given ID exists.
    async fn get(&self, id: Uuid) -> Result<Option<WebhookReceipt>, StorageError> {
        let sql = format!("{SELECT_COLUMNS} WHERE receipt_id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        row.map(|r| pg_row_to_receipt_row(&r).and_then(receipt_from_row))
            .transpose()
    }

    /// Transition the [`ProcessingState`] of an existing receipt.
    ///
    /// Stores an optional `error` string alongside the new state.
    async fn update_state(
        &self,
        id: Uuid,
        state: ProcessingState,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let state_str = serialize_enum(&state)?;
        let result = sqlx::query(
            r"
            UPDATE webhook_receipts
            SET processing_state = $1,
                last_error        = $2
            WHERE receipt_id = $3
            ",
        )
        .bind(&state_str)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        if result.rows_affected() == 0 {
            tracing::warn!(receipt_id = %id, "update_state: no receipt found with this id");
        }

        Ok(())
    }

    /// Query stored receipts using the supplied [`ReceiptFilter`].
    ///
    /// Applies optional filters on `provider_name`, `processing_state`,
    /// `external_reference`, and `provider_event_id`.  Results are ordered by
    /// `received_at DESC` and support `limit`/`offset` pagination.
    async fn query(&self, filter: ReceiptFilter) -> Result<Vec<WebhookReceipt>, StorageError> {
        let state_str: Option<String> = filter
            .processing_state
            .as_ref()
            .map(serialize_enum)
            .transpose()?;

        let sql = format!(
            r"
            {SELECT_COLUMNS}
            WHERE ($1::text IS NULL OR provider_name      = $1)
              AND ($2::text IS NULL OR processing_state   = $2)
              AND ($3::text IS NULL OR external_reference = $3)
              AND ($4::text IS NULL OR provider_event_id  = $4)
            ORDER BY received_at DESC
            LIMIT  $5
            OFFSET $6
            "
        );

        let rows = sqlx::query(&sql)
            .bind(filter.provider_name)
            .bind(state_str)
            .bind(filter.external_reference)
            .bind(filter.provider_event_id)
            .bind(filter.limit.unwrap_or(1000))
            .bind(filter.offset.unwrap_or(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        rows.iter()
            .map(|r| pg_row_to_receipt_row(r).and_then(receipt_from_row))
            .collect()
    }
}

/// Ops-specific queries not part of the core [`Storage`] trait.
impl PostgresStorage {
    /// Return up to 100 receipts eligible for retry.
    ///
    /// A receipt is eligible when its `processing_state` is `emit_failed` and
    /// its `emit_count` is below `max_attempts`.  Results are ordered oldest
    /// first so the retry worker processes them in FIFO order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the database query fails.
    pub async fn query_for_retry(
        &self,
        max_attempts: i32,
    ) -> Result<Vec<WebhookReceipt>, StorageError> {
        let sql = format!(
            "{SELECT_COLUMNS} WHERE processing_state = 'emit_failed' AND emit_count < $1 \
             ORDER BY received_at ASC LIMIT 100"
        );

        let rows = sqlx::query(&sql)
            .bind(max_attempts)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        rows.iter()
            .map(|r| pg_row_to_receipt_row(r).and_then(receipt_from_row))
            .collect()
    }

    /// Atomically increment `emit_count` and transition state after a failed
    /// emit attempt.
    ///
    /// If `emit_count + 1 >= max_attempts` the receipt is moved to
    /// `dead_lettered`; otherwise it stays in `emit_failed`.  The update is
    /// performed in a single SQL statement to avoid races.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the database query fails.
    pub async fn retry_failed(&self, id: Uuid, max_attempts: i32) -> Result<(), StorageError> {
        let result = sqlx::query(
            r"
            UPDATE webhook_receipts
            SET emit_count        = emit_count + 1,
                processing_state  = CASE
                    WHEN emit_count + 1 >= $2 THEN 'dead_lettered'
                    ELSE 'emit_failed'
                END
            WHERE receipt_id = $1
            ",
        )
        .bind(id)
        .bind(max_attempts)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        if result.rows_affected() == 0 {
            tracing::warn!(receipt_id = %id, "retry_failed: no receipt found with this id");
        }

        Ok(())
    }

    /// Reset a receipt so it will be picked up by the retry worker again.
    ///
    /// Sets `processing_state` back to `emit_failed` and clears `emit_count`
    /// to zero.  Used by CLI replay and dead-letter-queue retry commands.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the database query fails.
    pub async fn reset_for_retry(&self, id: Uuid) -> Result<(), StorageError> {
        let result = sqlx::query(
            r"
            UPDATE webhook_receipts
            SET processing_state = 'emit_failed',
                emit_count       = 0
            WHERE receipt_id = $1
            ",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(StorageError::Internal(format!(
                "no receipt found with id {id}"
            )));
        }

        Ok(())
    }

    /// Fetch receipts that share a given `external_reference`.
    ///
    /// Results are ordered newest first.  `limit` defaults to 50 when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the database query fails.
    pub async fn query_by_external_reference(
        &self,
        external_ref: &str,
        limit: Option<i64>,
    ) -> Result<Vec<WebhookReceipt>, StorageError> {
        let sql = format!(
            "{SELECT_COLUMNS} WHERE external_reference = $1 \
             ORDER BY received_at DESC LIMIT $2"
        );

        let rows = sqlx::query(&sql)
            .bind(external_ref)
            .bind(limit.unwrap_or(50))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        rows.iter()
            .map(|r| pg_row_to_receipt_row(r).and_then(receipt_from_row))
            .collect()
    }

    /// Fetch receipts in a failed terminal state received on or after `since`.
    ///
    /// Covers both `emit_failed` and `dead_lettered` states.  An optional
    /// `provider` filter narrows results to a single provider.  Results are
    /// ordered oldest first.  `limit` defaults to 100 when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Internal`] if the database query fails.
    pub async fn query_failed_since(
        &self,
        provider: Option<&str>,
        since: DateTime<Utc>,
        limit: Option<i64>,
    ) -> Result<Vec<WebhookReceipt>, StorageError> {
        let sql = format!(
            "{SELECT_COLUMNS} \
             WHERE processing_state IN ('emit_failed', 'dead_lettered') \
               AND received_at >= $1 \
               AND ($3::text IS NULL OR provider_name = $3) \
             ORDER BY received_at ASC LIMIT $2"
        );

        let rows = sqlx::query(&sql)
            .bind(since)
            .bind(limit.unwrap_or(100))
            .bind(provider)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        rows.iter()
            .map(|r| pg_row_to_receipt_row(r).and_then(receipt_from_row))
            .collect()
    }
}

/// Raw database row returned by `SELECT *` on `webhook_deliveries`.
struct DeliveryRow {
    delivery_id: Uuid,
    receipt_id: Uuid,
    emitter_name: String,
    state: String,
    attempt_count: i32,
    last_error: Option<String>,
    last_attempt_at: Option<DateTime<Utc>>,
    next_attempt_at: DateTime<Utc>,
    emitted_at: Option<DateTime<Utc>>,
    immutable: bool,
    created_at: DateTime<Utc>,
}

/// Map a raw `sqlx` [`sqlx::postgres::PgRow`] into a [`DeliveryRow`].
fn pg_row_to_delivery_row(r: &sqlx::postgres::PgRow) -> Result<DeliveryRow, StorageError> {
    Ok(DeliveryRow {
        delivery_id: r
            .try_get("delivery_id")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        receipt_id: r
            .try_get("receipt_id")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        emitter_name: r
            .try_get("emitter_name")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        state: r
            .try_get("state")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        attempt_count: r
            .try_get("attempt_count")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        last_error: r
            .try_get("last_error")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        last_attempt_at: r
            .try_get("last_attempt_at")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        next_attempt_at: r
            .try_get("next_attempt_at")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        emitted_at: r
            .try_get("emitted_at")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        immutable: r
            .try_get("immutable")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
        created_at: r
            .try_get("created_at")
            .map_err(|e| StorageError::Internal(e.to_string()))?,
    })
}

/// Convert a [`DeliveryRow`] (raw DB row) into a [`WebhookDelivery`].
fn delivery_from_row(row: DeliveryRow) -> Result<WebhookDelivery, StorageError> {
    let state: DeliveryState = deserialize_enum(&row.state)?;
    Ok(WebhookDelivery {
        delivery_id: DeliveryId(row.delivery_id),
        receipt_id: ReceiptId(row.receipt_id),
        emitter_name: row.emitter_name,
        state,
        attempt_count: row.attempt_count,
        last_error: row.last_error,
        last_attempt_at: row.last_attempt_at,
        next_attempt_at: row.next_attempt_at,
        emitted_at: row.emitted_at,
        immutable: row.immutable,
        created_at: row.created_at,
    })
}

/// SELECT query used by delivery storage helpers.
const SELECT_DELIVERY_COLUMNS: &str = r"
    SELECT delivery_id, receipt_id, emitter_name, state, attempt_count,
           last_error, last_attempt_at, next_attempt_at, emitted_at,
           immutable, created_at
    FROM webhook_deliveries
";

/// Storage operations for the per-emitter background dispatch workers.
///
/// All methods are scoped to a single `emitter_name` to keep each worker
/// independent and to keep query plans tight on the partial indexes.
#[async_trait]
pub trait DeliveryStorage: Send + Sync {
    /// Error type returned by all delivery storage operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Claim up to `batch_size` rows that are ready for dispatch:
    /// state IN ('pending', 'failed') AND `next_attempt_at` <= `now()` AND immutable = FALSE.
    /// Uses FOR UPDATE SKIP LOCKED so concurrent workers claim disjoint batches.
    async fn claim_pending(
        &self,
        emitter_name: &str,
        batch_size: i64,
    ) -> Result<Vec<(WebhookDelivery, WebhookReceipt)>, Self::Error>;

    /// Reclaim orphaned in-flight rows whose lease has expired.
    /// Returns the number of rows reclaimed (for metrics).
    async fn reclaim_expired(
        &self,
        emitter_name: &str,
        lease_duration: Duration,
    ) -> Result<u64, Self::Error>;

    /// Mark a delivery as successfully emitted.
    async fn mark_emitted(&self, delivery_id: DeliveryId) -> Result<(), Self::Error>;

    /// Mark a delivery as failed with updated attempt count and backoff target.
    async fn mark_failed(
        &self,
        delivery_id: DeliveryId,
        attempt_count: i32,
        next_attempt_at: DateTime<Utc>,
        last_error: &str,
    ) -> Result<(), Self::Error>;

    /// Mark a delivery as dead-lettered (terminal failure).
    async fn mark_dead_lettered(
        &self,
        delivery_id: DeliveryId,
        last_error: &str,
    ) -> Result<(), Self::Error>;

    /// Count dead-lettered rows for a given emitter (DLQ depth gauge).
    async fn count_dlq(&self, emitter_name: &str) -> Result<u64, Self::Error>;

    /// Count pending + failed rows ready to dispatch (pending gauge).
    async fn count_pending(&self, emitter_name: &str) -> Result<u64, Self::Error>;

    /// Count in-flight rows (in-flight gauge).
    async fn count_in_flight(&self, emitter_name: &str) -> Result<u64, Self::Error>;

    /// Insert a new pending delivery row for replay.
    /// Caller must verify `emitter_name` is currently configured before calling.
    async fn insert_replay(
        &self,
        receipt_id: ReceiptId,
        emitter_name: &str,
    ) -> Result<DeliveryId, Self::Error>;

    /// Fetch a single delivery row plus its parent receipt.
    async fn get_delivery(
        &self,
        delivery_id: DeliveryId,
    ) -> Result<Option<(WebhookDelivery, WebhookReceipt)>, Self::Error>;

    /// Fetch all delivery rows for a receipt, ordered by `created_at` ASC.
    async fn get_deliveries_for_receipt(
        &self,
        receipt_id: ReceiptId,
    ) -> Result<Vec<WebhookDelivery>, Self::Error>;

    /// List dead-lettered deliveries, optionally filtered by emitter.
    async fn list_dlq(
        &self,
        emitter_name: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<(WebhookDelivery, WebhookReceipt)>, Self::Error>;
}

#[async_trait]
impl DeliveryStorage for PostgresStorage {
    type Error = StorageError;

    async fn claim_pending(
        &self,
        emitter_name: &str,
        batch_size: i64,
    ) -> Result<Vec<(WebhookDelivery, WebhookReceipt)>, Self::Error> {
        let sql = r"
            WITH claimed AS (
                SELECT delivery_id
                FROM webhook_deliveries
                WHERE emitter_name = $1
                  AND state IN ('pending', 'failed')
                  AND next_attempt_at <= now()
                  AND immutable = FALSE
                ORDER BY next_attempt_at
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            )
            UPDATE webhook_deliveries d
            SET state = 'in_flight',
                last_attempt_at = now()
            FROM claimed
            WHERE d.delivery_id = claimed.delivery_id
            RETURNING
                d.delivery_id, d.receipt_id, d.emitter_name, d.state,
                d.attempt_count, d.last_error, d.last_attempt_at,
                d.next_attempt_at, d.emitted_at, d.immutable, d.created_at
            ";

        let rows = sqlx::query(sql)
            .bind(emitter_name)
            .bind(batch_size)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let delivery_row = pg_row_to_delivery_row(row)?;
            let receipt_id_uuid = delivery_row.receipt_id;
            let delivery = delivery_from_row(delivery_row)?;
            let receipt = self.get(receipt_id_uuid).await?.ok_or_else(|| {
                StorageError::Internal("delivery references missing receipt".to_string())
            })?;
            result.push((delivery, receipt));
        }
        Ok(result)
    }

    async fn reclaim_expired(
        &self,
        emitter_name: &str,
        lease_duration: Duration,
    ) -> Result<u64, Self::Error> {
        let lease_secs = lease_duration.as_secs_f64();
        let result = sqlx::query(
            r"
            UPDATE webhook_deliveries
            SET state = 'failed',
                last_error = COALESCE(last_error || ' ', '') || '[reclaimed: lease expired]',
                next_attempt_at = now()
            WHERE emitter_name = $1
              AND state = 'in_flight'
              AND last_attempt_at < now() - make_interval(secs => $2)
              AND immutable = FALSE
            ",
        )
        .bind(emitter_name)
        .bind(lease_secs)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

        Ok(result.rows_affected())
    }

    async fn mark_emitted(&self, delivery_id: DeliveryId) -> Result<(), Self::Error> {
        sqlx::query(
            r"
            UPDATE webhook_deliveries
            SET state = 'emitted', emitted_at = now(), last_error = NULL
            WHERE delivery_id = $1
            ",
        )
        .bind(delivery_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn mark_failed(
        &self,
        delivery_id: DeliveryId,
        attempt_count: i32,
        next_attempt_at: DateTime<Utc>,
        last_error: &str,
    ) -> Result<(), Self::Error> {
        sqlx::query(
            r"
            UPDATE webhook_deliveries
            SET state = 'failed',
                attempt_count = $2,
                next_attempt_at = $3,
                last_error = $4
            WHERE delivery_id = $1
            ",
        )
        .bind(delivery_id.0)
        .bind(attempt_count)
        .bind(next_attempt_at)
        .bind(last_error)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn mark_dead_lettered(
        &self,
        delivery_id: DeliveryId,
        last_error: &str,
    ) -> Result<(), Self::Error> {
        sqlx::query(
            r"
            UPDATE webhook_deliveries
            SET state = 'dead_lettered', last_error = $2
            WHERE delivery_id = $1
            ",
        )
        .bind(delivery_id.0)
        .bind(last_error)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn count_dlq(&self, emitter_name: &str) -> Result<u64, Self::Error> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM webhook_deliveries WHERE emitter_name = $1 AND state = 'dead_lettered'",
        )
        .bind(emitter_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        u64::try_from(count).map_err(|e| StorageError::Internal(e.to_string()))
    }

    async fn count_pending(&self, emitter_name: &str) -> Result<u64, Self::Error> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM webhook_deliveries WHERE emitter_name = $1 AND state IN ('pending', 'failed') AND immutable = FALSE",
        )
        .bind(emitter_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        u64::try_from(count).map_err(|e| StorageError::Internal(e.to_string()))
    }

    async fn count_in_flight(&self, emitter_name: &str) -> Result<u64, Self::Error> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM webhook_deliveries WHERE emitter_name = $1 AND state = 'in_flight' AND immutable = FALSE",
        )
        .bind(emitter_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        u64::try_from(count).map_err(|e| StorageError::Internal(e.to_string()))
    }

    async fn insert_replay(
        &self,
        receipt_id: ReceiptId,
        emitter_name: &str,
    ) -> Result<DeliveryId, Self::Error> {
        let delivery_id: Uuid = sqlx::query_scalar(
            r"
            INSERT INTO webhook_deliveries (delivery_id, receipt_id, emitter_name, state, next_attempt_at)
            VALUES (gen_random_uuid(), $1, $2, 'pending', now())
            RETURNING delivery_id
            ",
        )
        .bind(receipt_id.0)
        .bind(emitter_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(DeliveryId(delivery_id))
    }

    async fn get_delivery(
        &self,
        delivery_id: DeliveryId,
    ) -> Result<Option<(WebhookDelivery, WebhookReceipt)>, Self::Error> {
        let sql = format!("{SELECT_DELIVERY_COLUMNS} WHERE delivery_id = $1");
        let row = sqlx::query(&sql)
            .bind(delivery_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let delivery_row = pg_row_to_delivery_row(&row)?;
        let receipt_id_uuid = delivery_row.receipt_id;
        let delivery = delivery_from_row(delivery_row)?;
        let receipt = self.get(receipt_id_uuid).await?.ok_or_else(|| {
            StorageError::Internal("delivery references missing receipt".to_string())
        })?;
        Ok(Some((delivery, receipt)))
    }

    async fn get_deliveries_for_receipt(
        &self,
        receipt_id: ReceiptId,
    ) -> Result<Vec<WebhookDelivery>, Self::Error> {
        let sql =
            format!("{SELECT_DELIVERY_COLUMNS} WHERE receipt_id = $1 ORDER BY created_at ASC");
        let rows = sqlx::query(&sql)
            .bind(receipt_id.0)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        rows.iter()
            .map(|r| pg_row_to_delivery_row(r).and_then(delivery_from_row))
            .collect()
    }

    async fn list_dlq(
        &self,
        emitter_name: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<(WebhookDelivery, WebhookReceipt)>, Self::Error> {
        let rows = if let Some(name) = emitter_name {
            let sql = format!(
                "{SELECT_DELIVERY_COLUMNS} WHERE state = 'dead_lettered' AND emitter_name = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
            );
            sqlx::query(&sql)
                .bind(name)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?
        } else {
            let sql = format!(
                "{SELECT_DELIVERY_COLUMNS} WHERE state = 'dead_lettered' ORDER BY created_at DESC LIMIT $1 OFFSET $2"
            );
            sqlx::query(&sql)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?
        };

        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            let delivery_row = pg_row_to_delivery_row(row)?;
            let receipt_id_uuid = delivery_row.receipt_id;
            let delivery = delivery_from_row(delivery_row)?;
            let receipt = self.get(receipt_id_uuid).await?.ok_or_else(|| {
                StorageError::Internal("delivery references missing receipt".to_string())
            })?;
            result.push((delivery, receipt));
        }
        Ok(result)
    }
}
