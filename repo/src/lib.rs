//! Repository layer for database access
//!
//! This crate provides a type-safe, async database abstraction layer using sqlx.
//! It includes connection pool management, strongly-typed models, and generic
//! repository implementations.
//!
//! # Features
//!
//! - **Connection Pool Management**: Centralized pool creation with configurable options
//! - **Type-Safe Models**: Structs that mirror database tables with proper type mappings
//! - **Generic Repository Pattern**: Methods accept any `Executor`, supporting both pools and transactions
//! - **Append-Only Tables**: Some tables (like bluetooth_occurrences) are immutable after insertion
//!
//! # Example
//!
//! ```no_run
//! use repo::{Pool, BluetoothOccurrenceRepository, BluetoothOccurrence};
//! use chrono::Utc;
//! use uuid::Uuid;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create connection pool
//!     let pool = Pool::connect("postgres://user:pass@localhost:7789/dbname").await?;
//!     let pg_pool = pool.as_pool().clone();
//!
//!     // Create a new occurrence
//!     let occurrence = BluetoothOccurrence::new(
//!         Uuid::now_v7(&[]).to_string(),
//!         "node-001".to_string(),
//!         Utc::now(),
//!         "AA:BB:CC:DD:EE:FF".to_string(),
//!     )
//!     .with_rssi(-67)
//!     .with_advertisement_type("ADV_IND");
//!
//!     // Insert (append-only)
//!     let saved = BluetoothOccurrenceRepository::create(&pg_pool, &occurrence).await?;
//!
//!     // Query
//!     let found = BluetoothOccurrenceRepository::find_by_id(&pg_pool, &saved.occurrence_id).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! # Transaction Support
//!
//! All repository methods accept any type implementing `Executor`, allowing them
//! to work within transactions:
//!
//! ```ignore
//! let mut tx = pool.begin().await?;
//! BluetoothOccurrenceRepository::create(&mut tx, &occurrence).await?;
//! tx.commit().await?;
//! ```

pub mod error;
pub mod models;
pub mod pool;
pub mod repositories;

// Re-export main types for convenience
pub use error::RepoError;
pub use models::BluetoothOccurrence;
pub use pool::Pool;
pub use repositories::BluetoothOccurrenceRepository;
