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
//! - **Append-Only Tables**: The occurrences table is immutable after insertion
//! - **Multi-Signal Support**: Unified model for Bluetooth, WiFi, and future signal types
//! - **Builder Pattern**: Fluent API for constructing complex occurrence records
//!
//! # Example
//!
//! ```no_run
//! use repo::{Pool, Occurrence, SignalType};
//! use chrono::Utc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create connection pool
//!     let pool = Pool::connect("postgres://user:pass@localhost:7789/dbname").await?;
//!     let pg_pool = pool.as_pool().clone();
//!
//!     // Create a new Bluetooth occurrence using the builder
//!     let node_id = vec![0u8; 32]; // 32-byte SHA-256 hash
//!     let device_hash = vec![1u8; 32]; // 32-byte SHA-256 hash
//!     let signed_payload = vec![2u8; 32];
//!     let signature = vec![3u8; 64];
//!     let mac = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
//!
//!     let occurrence = Occurrence::builder()
//!         .signal_type(SignalType::Bluetooth)
//!         .origin_node_id(&node_id)
//!         .observed_at(Utc::now())
//!         .observed_at_node_local(Utc::now())
//!         .device_hash(&device_hash)
//!         .rssi(-67)
//!         .signal_payload(serde_json::json!({}))
//!         .signed_payload(&signed_payload)
//!         .signature(&signature)
//!         .device_address(&mac)
//!         .build();
//!
//!     // Insert (append-only) - create your own OccurrenceRepository
//!     // let saved = OccurrenceRepository::create(&pg_pool, &occurrence).await?;
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
//! OccurrenceRepository::create(&mut tx, &occurrence).await?;
//! tx.commit().await?;
//! ```

pub mod error;
pub mod models;
pub mod pool;
pub mod repositories;
pub mod types;

// Re-export main types for convenience
pub use error::RepoError;
pub use models::{mac_address_from_string, Occurrence, OccurrenceBuilder, OccurrenceRelay, SignalType};
pub use pool::Pool;
pub use types::PostgisPoint;
pub use repositories::OccurrenceRepository;
