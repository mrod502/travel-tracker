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
//! Repository methods accept any type implementing `Executor`, allowing them
//! to work within transactions:
//!
//! ```ignore
//! let mut tx = pool.begin().await?;
//! OccurrenceRepository::insert_once(&mut tx, &occurrence).await?;
//! tx.commit().await?;
//! ```
//!
//! The one exception is [`OccurrenceRepository::create`], which needs its
//! executor twice — once for the INSERT, once to create a missing monthly
//! partition before retrying — so it takes a pool rather than a transaction.
//! Inside a transaction use `insert_once`, having called `ensure_partition`
//! first if `observed_at` could fall outside the months the schema has
//! provisioned.

pub mod error;
pub mod geo;
pub mod models;
pub mod pool;
pub mod repositories;
pub mod types;

// Re-export main types for convenience
pub use error::RepoError;
/// The H3 types the workspace speaks, re-exported so `h3o` stays a dependency of
/// this crate alone: one version pinned in one place, one spelling of "an H3
/// cell" everywhere else.
pub use h3o::{CellIndex, Resolution};
pub use models::{
    mac_address_from_string, Node, Occurrence, OccurrenceBuilder, OccurrenceRelay, RevokedNode,
    SignalType,
};
pub use pool::Pool;
pub use repositories::{NodeRepository, OccurrenceRepository, RevocationRepository};
pub use types::{H3Index, PostgisPoint};
