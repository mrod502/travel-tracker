//! FullNode implementation for Phase 0.
//!
//! This module provides the complete Phase 0 node implementation that:
//! 1. Monitors Bluetooth devices
//! 2. Signs occurrences with node identity
//! 3. Rate-limits storage to avoid duplicates
//! 4. Stores signed occurrences in the database
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐
//! │ Bluetooth Scan  │ → DeviceEvent → store_occurrence()
//! └─────────────────┘                  │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Rate Limiter     │ → Skip if limited
//!                             └──────────────────┘
//!                                      │ (pass)
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Build Payload    │
//!                             └──────────────────┘
//!                                      │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ CBOR Encode      │
//!                             └──────────────────┘
//!                                      │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Sign with Key    │
//!                             └──────────────────┘
//!                                      │
//!                                      ▼
//!                             ┌──────────────────┐
//!                             │ Store to DB      │
//!                             └──────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use app::node::full::FullNode;
//! use std::path::PathBuf;
//!
//! let data_dir = PathBuf::from("/var/lib/btmon");
//! let mut node = FullNode::new(data_dir).await?;
//! node.run().await?;
//! ```

use bt_mon::{BluetoothDevice, DeviceEvent, DeviceMonitor};
use bt_mon::monitor::events::UpdateField;
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use repo::{Occurrence, OccurrenceRepository, Pool, SignalType};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::node::{Clock, Node, RateLimiter, RateLimiterConfig, RateLimiterStats, SystemClock};
use crate::provenance::encode::encode_payload;
use crate::provenance::payload::CanonicalPayload;
use crate::node::identity::NodeIdentity;

/// Configuration for FullNode.
#[derive(Clone)]
pub struct FullNodeConfig {
    /// Database connection pool.
    pub pool: Pool,
    
    /// Data directory for node identity and other persistent state.
    pub data_dir: PathBuf,
    
    /// Rate limiter threshold (default: 15 seconds).
    pub rate_limit_threshold_ms: u64,
    
    /// Optional max cache size for rate limiter.
    pub rate_limit_max_cache_size: Option<usize>,
    
    /// Fixed location (latitude, longitude) if node has no GPS.
    /// If None, location is not included in occurrences.
    pub fixed_location: Option<(f64, f64)>,
}

impl std::fmt::Debug for FullNodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FullNodeConfig")
            .field("pool", &"<Pool>")
            .field("data_dir", &self.data_dir)
            .field("rate_limit_threshold_ms", &self.rate_limit_threshold_ms)
            .field("rate_limit_max_cache_size", &self.rate_limit_max_cache_size)
            .field("fixed_location", &self.fixed_location)
            .finish()
    }
}

impl FullNodeConfig {
    /// Create a new configuration with default values.
    pub fn new(pool: Pool, data_dir: PathBuf) -> Self {
        Self {
            pool,
            data_dir,
            rate_limit_threshold_ms: 15_000, // 15 seconds default
            rate_limit_max_cache_size: None,
            fixed_location: None,
        }
    }

    /// Set the rate limit threshold.
    pub fn with_rate_limit_threshold(mut self, threshold_ms: u64) -> Self {
        self.rate_limit_threshold_ms = threshold_ms;
        self
    }

    /// Set the max cache size for the rate limiter.
    pub fn with_rate_limit_max_cache_size(mut self, size: usize) -> Self {
        self.rate_limit_max_cache_size = Some(size);
        self
    }

    /// Set a fixed location for the node.
    pub fn with_fixed_location(mut self, lat: f64, lon: f64) -> Self {
        self.fixed_location = Some((lat, lon));
        self
    }
}

/// Statistics about FullNode operation.
#[derive(Debug, Clone)]
pub struct FullNodeStats {
    /// Total number of device events received.
    pub total_events: usize,
    
    /// Number of occurrences stored.
    pub occurrences_stored: usize,
    
    /// Number of occurrences rate-limited.
    pub occurrences_rate_limited: usize,
    
    /// Number of storage errors.
    pub storage_errors: usize,
    
    /// Rate limiter statistics.
    pub rate_limiter_stats: RateLimiterStats,
}

/// The Phase 0 FullNode implementation.
///
/// A FullNode is a standalone node that:
/// - Monitors Bluetooth devices on its local adapter
/// - Signs all occurrences with its node identity
/// - Rate-limits storage to avoid duplicates
/// - Stores signed occurrences in its local database
///
/// # Thread Safety
///
/// FullNode is `Send + Sync` and can be used from multiple threads.
/// The internal state (rate limiter, counters) is protected by atomics and DashMap.
pub struct FullNode {
    /// Node identity (key pair for signing).
    identity: NodeIdentity,
    
    /// Database connection pool.
    pool: Pool,
    
    /// Rate limiter for deduplication.
    rate_limiter: Arc<RateLimiter>,
    
    /// Clock for timestamps.
    clock: Arc<dyn Clock>,
    
    /// Fixed location if configured.
    fixed_location: Option<(f64, f64)>,
    
    /// Statistics counters.
    total_events: std::sync::atomic::AtomicUsize,
    occurrences_stored: std::sync::atomic::AtomicUsize,
    occurrences_rate_limited: std::sync::atomic::AtomicUsize,
    storage_errors: std::sync::atomic::AtomicUsize,
}

impl FullNode {
    /// Create a new FullNode instance.
    ///
    /// This loads or creates the node identity, initializes the rate limiter,
    /// and sets up the database connection.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for the node
    ///
    /// # Returns
    ///
    /// * `Ok(FullNode)` - A fully initialized node
    /// * `Err(AppError)` - If initialization failed
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::full::{FullNode, FullNodeConfig};
    /// use repo::Pool;
    /// use std::path::PathBuf;
    ///
    /// let pool = Pool::connect("postgres://localhost/test").await?;
    /// let config = FullNodeConfig::new(pool, PathBuf::from("/var/lib/btmon"))
    ///     .with_rate_limit_threshold_ms(15_000);
    ///
    /// let node = FullNode::new(config).await?;
    /// ```
    pub async fn new(config: FullNodeConfig) -> Result<Self> {
        // Load or create node identity
        let identity = NodeIdentity::load_or_create(&config.data_dir)
            .map_err(|e| AppError::Io(format!("Failed to load node identity: {}", e)))?;
        
        info!("Node identity loaded/created. Node ID: {}", hex::encode(identity.node_id()));
        
        // Create rate limiter
        let rate_limiter_config = RateLimiterConfig::with_threshold(Duration::from_millis(config.rate_limit_threshold_ms))
            .with_max_cache_size(config.rate_limit_max_cache_size.unwrap_or(usize::MAX));
        let rate_limiter = Arc::new(RateLimiter::with_config(rate_limiter_config));
        
        Ok(Self {
            identity,
            pool: config.pool,
            rate_limiter,
            clock: Arc::new(SystemClock),
            fixed_location: config.fixed_location,
            total_events: std::sync::atomic::AtomicUsize::new(0),
            occurrences_stored: std::sync::atomic::AtomicUsize::new(0),
            occurrences_rate_limited: std::sync::atomic::AtomicUsize::new(0),
            storage_errors: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Run the FullNode, monitoring Bluetooth devices and storing occurrences.
    ///
    /// This method blocks until the device event stream is closed (e.g., Ctrl+C).
    ///
    /// # Arguments
    ///
    /// * `monitor` - A Bluetooth device monitor (must implement DeviceMonitor)
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The node stopped cleanly
    /// * `Err(AppError)` - If an error occurred
    ///
    /// # Example
    ///
    /// ```ignore
    /// use app::node::full::{FullNode, FullNodeConfig};
    /// use bt_mon::create_btleplug_monitor;
    ///
    /// let config = FullNodeConfig::new(pool, data_dir);
    /// let node = FullNode::new(config).await?;
    /// let monitor = create_btleplug_monitor().await?;
    ///
    /// node.run(monitor).await?;
    /// ```
    pub async fn run<M: DeviceMonitor + Send>(&self, monitor: M) -> Result<()> {
        info!("Starting Bluetooth monitoring...");
        
        // Check if adapter is powered
        let powered = monitor
            .is_powered()
            .await
            .map_err(AppError::Bluetooth)?;
        if !powered {
            warn!("Bluetooth adapter is not powered on");
        } else {
            info!("Bluetooth adapter is powered on");
        }
        
        // Start scanning
        monitor
            .start_scan()
            .await
            .map_err(AppError::Bluetooth)?;
        info!("Scan started");
        
        // Get event stream
        let mut events = monitor
            .device_events()
            .await
            .map_err(AppError::Bluetooth)?;
        
        info!("Listening for Bluetooth events (press Ctrl+C to stop)...");
        
        // Main event loop
        while let Some(event) = events.next().await {
            self.total_events.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            
            match event {
                DeviceEvent::DeviceAdded { device } => {
                    info!("Discovered device: {}", device.id);
                    if let Err(e) = self.store_occurrence(&device).await {
                        error!("Error storing occurrence: {}", e);
                        self.storage_errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                
                DeviceEvent::DeviceRemoved { id } => {
                    debug!("Device removed: {}", id);
                }
                
                DeviceEvent::DeviceUpdated {
                    device,
                    changed_fields,
                } => {
                    debug!(
                        "Device updated: {} (changed: {:?})",
                        device.id, changed_fields
                    );
                    
                    // Handle RSSI updates - store as new occurrence
                    if changed_fields.contains(&UpdateField::Rssi) {
                        if let Err(e) = self.store_occurrence(&device).await {
                            error!("Error storing occurrence: {}", e);
                            self.storage_errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
            }
        }
        
        info!("Event stream closed");
        
        // Stop scanning
        monitor
            .stop_scan()
            .await
            .map_err(AppError::Bluetooth)?;
        info!("Scan stopped");
        
        Ok(())
    }

    /// Store an occurrence for a Bluetooth device.
    ///
    /// This method:
    /// 1. Computes the device hash (SHA-256 of MAC)
    /// 2. Checks the rate limiter (skips if limited)
    /// 3. Builds a canonical payload
    /// 4. CBOR encodes the payload
    /// 5. Signs the encoded bytes
    /// 6. Inserts the occurrence into the database
    ///
    /// # Arguments
    ///
    /// * `device` - The Bluetooth device to store
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The occurrence was stored (or rate-limited)
    /// * `Err(AppError)` - If storage failed
    async fn store_occurrence(&self, device: &BluetoothDevice) -> Result<()> {
        // Parse MAC address
        let device_address = repo::mac_address_from_string(device.id.as_str())
            .map_err(|e| AppError::InvalidMacAddress(e.to_string()))?;
        
        // Check rate limiter
        if !self.rate_limiter.should_store(&device_address) {
            debug!("Rate limited: {}", device.id);
            self.occurrences_rate_limited.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        
        // Generate timestamps
        let _occurrence_id = Uuid::now_v7();
        let observed_at = self.clock.now();
        let observed_at_node_local = self.clock.now_local();
        
        // Compute device hash (SHA-256 of MAC address)
        let device_hash = {
            let mut hasher = Sha256::new();
            hasher.update(&device_address);
            hasher.finalize().to_vec()
        };
        
        // Build canonical payload
        let payload = CanonicalPayload::builder()
            .signal_type(0) // Bluetooth
            .origin_node_id(self.identity.node_id())
            .device_hash(&device_hash)
            .device_address(&device_address)
            .observed_at_node_local(&observed_at_node_local.to_rfc3339())
            .rssi(device.rssi.unwrap_or(0) as i16)
            .build();
        
        // Encode to CBOR
        let encoded = encode_payload(&payload)
            .map_err(|e| AppError::Validation(format!("CBOR encoding failed: {}", e)))?;
        
        // Sign the encoded bytes
        let signature = self.identity.sign(&encoded);
        
        // Build signal payload with Bluetooth-specific data
        let signal_payload = self.build_ble_payload(device, &device_address)?;
        
        // Build occurrence
        let occurrence = Occurrence::builder()
            .signal_type(SignalType::Bluetooth)
            .origin_node_id(self.identity.node_id())
            .observed_at(observed_at)
            .observed_at_node_local(observed_at_node_local)
            .device_address(&device_address)
            .device_hash(&device_hash)
            .rssi(device.rssi.unwrap_or(0) as i16)
            .signal_payload(signal_payload)
            .signed_payload(&encoded)
            .signature(&signature.to_bytes())
            .build();
        
        // Insert into database
        match OccurrenceRepository::create(self.pool.as_pool(), &occurrence).await {
            Ok(_) => {
                self.occurrences_stored.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                debug!("Stored occurrence for device: {}", device.id);
            }
            Err(e) => {
                // Check if it's a duplicate (shouldn't happen with UUIDv7)
                error!("Failed to store occurrence: {}", e);
                return Err(AppError::Database(e));
            }
        }
        
        Ok(())
    }

    /// Build a Bluetooth-specific signal payload.
    fn build_ble_payload(
        &self,
        device: &BluetoothDevice,
        device_address: &[u8],
    ) -> Result<serde_json::Value> {
        let mut ble_payload = serde_json::Map::new();
        
        // Address type (default to public)
        ble_payload.insert(
            "address_type".to_string(),
            serde_json::json!("public"),
        );
        
        // Device address as hex string
        ble_payload.insert(
            "address".to_string(),
            serde_json::json!(hex::encode(device_address)),
        );
        
        // Handle manufacturer data
        if let Some((company_id, payload)) = device.manufacturer_data.iter().next() {
            ble_payload.insert(
                "manufacturer_data".to_string(),
                serde_json::json!({
                    "company_id": company_id,
                    "payload": hex::encode(payload)
                }),
            );
        }
        
        // Handle service UUIDs
        if !device.service_data.is_empty() {
            let uuids: Vec<String> = device
                .service_data
                .keys()
                .map(|u| u.as_uuid().to_string())
                .collect();
            ble_payload.insert("service_uuids".to_string(), serde_json::json!(uuids));
        }
        
        // Add RSSI if available
        if let Some(rssi) = device.rssi {
            ble_payload.insert("rssi".to_string(), serde_json::json!(rssi));
        }
        
        // Add device name if available
        if let Some(name) = &device.name {
            ble_payload.insert("name".to_string(), serde_json::json!(name));
        }
        
        // Add services resolved flag
        ble_payload.insert(
            "services_resolved".to_string(),
            serde_json::json!(device.services_resolved),
        );
        
        // Wrap in signal_type key
        let mut signal_payload = serde_json::Map::new();
        signal_payload.insert("ble".to_string(), serde_json::json!(ble_payload));
        
        Ok(serde_json::Value::Object(signal_payload))
    }

    /// Get the node's unique ID.
    ///
    /// This is a 32-byte SHA-256 hash of the signing public key.
    pub fn node_id(&self) -> &[u8] {
        self.identity.node_id()
    }

    /// Get statistics about node operation.
    pub fn stats(&self) -> FullNodeStats {
        FullNodeStats {
            total_events: self.total_events.load(std::sync::atomic::Ordering::SeqCst),
            occurrences_stored: self.occurrences_stored.load(std::sync::atomic::Ordering::SeqCst),
            occurrences_rate_limited: self.occurrences_rate_limited.load(std::sync::atomic::Ordering::SeqCst),
            storage_errors: self.storage_errors.load(std::sync::atomic::Ordering::SeqCst),
            rate_limiter_stats: self.rate_limiter.stats(),
        }
    }

    /// Get the rate limit threshold in milliseconds.
    pub fn rate_limit_threshold_ms(&self) -> u64 {
        self.rate_limiter.stats().threshold_ms
    }

    /// Clear the rate limiter cache.
    ///
    /// This is primarily useful for testing.
    pub fn clear_rate_limiter(&self) {
        self.rate_limiter.clear();
    }

    /// Verify a signature from this node.
    ///
    /// # Arguments
    ///
    /// * `payload` - The bytes that were signed
    /// * `signature` - The signature to verify
    ///
    /// # Returns
    ///
    /// * `Ok(())` - The signature is valid
    /// * `Err(VerifyError)` - If verification fails
    pub fn verify(&self, payload: &[u8], signature: &ed25519_dalek::Signature) -> Result<()> {
        self.identity.verify(payload, signature)
            .map_err(|e| AppError::Validation(e.to_string()))
    }
}

impl Node for FullNode {
    fn node_id(&self) -> &[u8] {
        self.identity.node_id()
    }

    fn sign(&self, payload: &[u8]) -> ed25519_dalek::Signature {
        self.identity.sign(payload)
    }

    fn verify(&self, payload: &[u8], signature: &ed25519_dalek::Signature) -> crate::provenance::verify::Result<()> {
        self.identity.verify(payload, signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_node_stats_clone() {
        let stats = FullNodeStats {
            total_events: 100,
            occurrences_stored: 80,
            occurrences_rate_limited: 20,
            storage_errors: 0,
            rate_limiter_stats: RateLimiterStats {
                cache_size: 50,
                allow_count: 80,
                deny_count: 20,
                cache_hit_rate: 20.0,
                threshold_ms: 15_000,
            },
        };
        
        let cloned = stats.clone();
        assert_eq!(cloned.total_events, 100);
        assert_eq!(cloned.occurrences_stored, 80);
        assert_eq!(cloned.occurrences_rate_limited, 20);
    }

    #[test]
    fn test_stats_display() {
        let stats = FullNodeStats {
            total_events: 100,
            occurrences_stored: 80,
            occurrences_rate_limited: 20,
            storage_errors: 1,
            rate_limiter_stats: RateLimiterStats {
                cache_size: 50,
                allow_count: 80,
                deny_count: 20,
                cache_hit_rate: 20.0,
                threshold_ms: 15_000,
            },
        };
        
        let _display = format!("{:?}", stats);
    }
}
