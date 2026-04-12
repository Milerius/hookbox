//! `PostgreSQL` storage backend for hookbox.
//!
//! Provides [`PostgresStorage`] (implements [`hookbox::traits::Storage`]) and
//! [`StorageDedupe`] (implements [`hookbox::traits::DedupeStrategy`]) backed by
//! a `sqlx` connection pool.
//!
//! # Quick start
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use sqlx::PgPool;
//! use hookbox_postgres::{PostgresStorage, StorageDedupe};
//!
//! let pool = PgPool::connect("postgres://localhost/hookbox").await?;
//! let storage = PostgresStorage::new(pool.clone());
//! storage.migrate().await?;
//!
//! let dedupe = StorageDedupe::new(pool);
//! # Ok(())
//! # }
//! ```

pub mod dedupe;
pub mod storage;

pub use dedupe::StorageDedupe;
pub use storage::{DeliveryStorage, PostgresStorage};
