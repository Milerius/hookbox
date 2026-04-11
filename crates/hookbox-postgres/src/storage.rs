//! `PostgreSQL`-backed implementation of the [`Storage`] trait.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use hookbox::error::StorageError;
use hookbox::state::{ProcessingState, ReceiptId, StoreResult, VerificationStatus};
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
            tracing::warn!(receipt_id = %id, "reset_for_retry: no receipt found with this id");
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
