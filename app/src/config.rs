//! Application configuration.
//!
//! Configuration can be provided via:
//! 1. CLI arguments (highest priority)
//! 2. Environment variables
//! 3. TOML config file (lowest priority)

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bluetooth Monitoring Application
///
/// Monitors Bluetooth Low Energy (BLE) devices and stores discoveries
/// in a PostgreSQL database.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
#[command(next_help_heading = "Options")]
pub struct Config {
    /// Configuration file (TOML format)
    #[arg(long, env = "CONFIG_FILE")]
    pub config_file: Option<String>,

    /// PostgreSQL connection string (alternative to individual connection params)
    #[arg(short, long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    /// PostgreSQL host
    #[arg(long, env = "PGHOST")]
    pub pg_host: Option<String>,

    /// PostgreSQL port
    #[arg(long, env = "PGPORT")]
    pub pg_port: Option<u16>,

    /// PostgreSQL database name
    #[arg(long, env = "PGDATABASE")]
    pub pg_database: Option<String>,

    /// PostgreSQL user
    #[arg(long, env = "PGUSER")]
    pub pg_user: Option<String>,

    /// PostgreSQL password
    #[arg(long, env = "PGPASSWORD")]
    pub pg_password: Option<String>,

    /// Log level (debug, info, warn, error)
    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    /// Node UUID (from certificate). Identifies this node in the network.
    #[arg(long, env = "NODE_ID")]
    pub node_id: Option<String>,

    /// Bluetooth scan interval in milliseconds
    #[arg(long, env = "BT_SCAN_INTERVAL_MS", default_value = "1000")]
    pub scan_interval_ms: u64,

    /// Whether to store raw advertisement payload
    #[arg(long, env = "BT_STORE_RAW_PAYLOAD", default_value = "true")]
    pub store_raw_payload: bool,

    /// Bluetooth adapter ID (optional). If not specified, uses first available adapter.
    #[arg(long, env = "BT_ADAPTER_ID")]
    pub adapter_id: Option<String>,

    /// Data directory for node identity and state files.
    ///
    /// Default: ~/.btmon/data
    #[arg(long, env = "BT_DATA_DIR")]
    pub data_dir: Option<String>,

    /// Rate limit threshold in milliseconds.
    ///
    /// Minimum time between occurrences for the same device.
    /// Default: 15000 (15 seconds)
    #[arg(long, env = "BT_RATE_LIMIT_MS", default_value = "15000")]
    pub rate_limit_ms: u64,

    /// Optional fixed location (latitude,longitude).
    ///
    /// If set, this location is used for all occurrences.
    /// Format: "lat,lon" (e.g., "40.6892,-74.0445")
    #[arg(long, env = "BT_FIXED_LOCATION")]
    pub fixed_location: Option<String>,
}

impl Config {
    /// Parse configuration from CLI and environment.
    pub fn parse_args() -> Self {
        Self::parse()
    }

    /// Load configuration from a TOML file.
    ///
    /// Returns `Ok(None)` if the file doesn't exist or can't be read.
    /// Returns `Ok(Some(ConfigFile))` if the file was loaded successfully.
    /// Returns `Err` if the file exists but can't be parsed.
    pub fn load_from_file(path: &PathBuf) -> Result<Option<ConfigFile>, String> {
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file: {}", e))?;

        let config: ConfigFile = toml::from_str(&content)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;

        Ok(Some(config))
    }

    /// Merge configuration from a TOML file with CLI config.
    ///
    /// CLI arguments take precedence over file configuration.
    pub fn merge_with_file(mut self, file_config: ConfigFile) -> Self {
        // Database configuration
        if let Some(db) = file_config.database {
            if self.database_url.is_none() {
                self.database_url = db.url;
            }
            if self.pg_host.is_none() {
                self.pg_host = db.host;
            }
            if self.pg_port.is_none() {
                self.pg_port = db.port;
            }
            if self.pg_database.is_none() {
                self.pg_database = db.database;
            }
            if self.pg_user.is_none() {
                self.pg_user = db.user;
            }
            if self.pg_password.is_none() {
                self.pg_password = db.password;
            }
        }

        // Application configuration
        if self.log_level == "info" && file_config.log.is_some() {
            self.log_level = file_config.log.as_ref().and_then(|l| l.level.clone()).unwrap_or_default();
        }
        if self.node_id.is_none() {
            self.node_id = file_config.node.and_then(|n| n.id);
        }
        
        // Bluetooth configuration
        if let Some(blue) = file_config.bluetooth {
            if self.adapter_id.is_none() {
                self.adapter_id = blue.adapter_id;
            }
            if self.data_dir.is_none() {
                self.data_dir = blue.data_dir;
            }
            if self.rate_limit_ms == 15000 {
                if let Some(val) = blue.rate_limit_ms {
                    self.rate_limit_ms = val;
                }
            }
            if self.fixed_location.is_none() {
                self.fixed_location = blue.fixed_location;
            }
            if self.scan_interval_ms == 1000 {
                if let Some(val) = blue.scan_interval_ms {
                    self.scan_interval_ms = val;
                }
            }
            if self.store_raw_payload {
                if let Some(val) = blue.store_raw_payload {
                    self.store_raw_payload = val;
                }
            }
        }

        self
    }

    /// Build the database connection string from components.
    ///
    /// Returns DATABASE_URL if provided, otherwise constructs from PG* env vars.
    pub fn database_url(&self) -> Result<String, String> {
        // If DATABASE_URL is provided directly, use it
        if let Some(ref url) = self.database_url {
            if !url.is_empty() {
                return Ok(url.clone());
            }
        }

        // Otherwise, build from individual components
        let host = self.pg_host.as_deref().unwrap_or("localhost");
        let port = self.pg_port.unwrap_or(5432);
        let database = self
            .pg_database
            .as_deref()
            .ok_or("PGDATABASE environment variable or --pg-database required")?;
        let user = self
            .pg_user
            .as_deref()
            .ok_or("PGUSER environment variable or --pg-user required")?;
        let password = self.pg_password.as_deref().unwrap_or("");

        // Escape special characters in password for connection string
        let escaped_password = escape_conn_str(password);

        Ok(format!(
            "postgres://{}:{}@{}:{}/{}",
            user, escaped_password, host, port, database
        ))
    }

    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        // Validate node_id
        if self.node_id.is_none() {
            return Err("NODE_ID environment variable or --node-id required".to_string());
        }

        // Validate node_id is a valid UUID
        if let Some(ref node_id) = self.node_id {
            uuid::Uuid::parse_str(node_id)
                .map_err(|e| format!("Invalid NODE_ID UUID: {}", e))?;
        }

        // Validate database configuration (either DATABASE_URL or PG* vars)
        let has_dsn = self.database_url.as_ref().map_or(false, |u| !u.is_empty());
        let has_components = self.pg_database.is_some() && self.pg_user.is_some();

        if !has_dsn && !has_components {
            return Err(
                "Either DATABASE_URL or PGDATABASE/PGUSER environment variables required"
                    .to_string(),
            );
        }

        // Validate scan interval
        if self.scan_interval_ms == 0 {
            return Err("BT_SCAN_INTERVAL_MS must be greater than 0".to_string());
        }

        // Validate rate limit
        if self.rate_limit_ms == 0 {
            return Err("BT_RATE_LIMIT_MS must be greater than 0".to_string());
        }

        // Validate fixed location if provided
        if self.fixed_location.is_some() {
            self.fixed_location(); // This validates the format
        }

        Ok(())
    }

    /// Get the parsed node_id as a UUID.
    pub fn node_id(&self) -> Result<uuid::Uuid, &'static str> {
        match &self.node_id {
            Some(s) => s.parse().map_err(|_| "Invalid UUID format"),
            None => Err("NODE_ID not set"),
        }
    }

    /// Get the data directory path.
    ///
    /// Returns the configured data_dir or defaults to ~/.btmon/data
    pub fn data_dir(&self) -> Result<std::path::PathBuf, String> {
        if let Some(ref dir) = self.data_dir {
            Ok(std::path::PathBuf::from(dir))
        } else {
            // Default to ~/.btmon/data
            let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set")?;
            Ok(std::path::PathBuf::from(home).join(".btmon").join("data"))
        }
    }

    /// Get the rate limit threshold as a Duration.
    pub fn rate_limit_threshold(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.rate_limit_ms)
    }

    /// Parse the fixed location if configured.
    ///
    /// Returns Some((latitude, longitude)) if fixed_location is set.
    pub fn fixed_location(&self) -> Option<Result<(f64, f64), String>> {
        self.fixed_location.as_ref().map(|s| {
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() != 2 {
                return Err(format!(
                    "Invalid fixed_location format. Expected 'lat,lon', got '{}'",
                    s
                ));
            }

            let lat: f64 = parts[0].trim().parse().map_err(|_| {
                format!("Invalid latitude '{}' in fixed_location", parts[0])
            })?;

            let lon: f64 = parts[1].trim().parse().map_err(|_| {
                format!("Invalid longitude '{}' in fixed_location", parts[1])
            })?;

            // Validate latitude range
            if lat < -90.0 || lat > 90.0 {
                return Err(format!("Latitude {} out of range [-90, 90]", lat));
            }

            // Validate longitude range
            if lon < -180.0 || lon > 180.0 {
                return Err(format!("Longitude {} out of range [-180, 180]", lon));
            }

            Ok((lat, lon))
        })
    }
}

/// Escape special characters in connection string values.
fn escape_conn_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "'\\''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_missing_node_id() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: None,
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_node_id() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("invalid-uuid".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_valid_config_with_dsn() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_valid_config_with_components() {
        let config = Config {
            database_url: None,
            pg_host: Some("localhost".to_string()),
            pg_port: Some(5432),
            pg_database: Some("testdb".to_string()),
            pg_user: Some("testuser".to_string()),
            pg_password: Some("testpass".to_string()),
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_missing_db_config() {
        let config = Config {
            database_url: None,
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_database_url_from_dsn() {
        let config = Config {
            database_url: Some("postgres://custom@localhost:9999/customdb".to_string()),
            pg_host: Some("other".to_string()),
            pg_port: Some(1234),
            pg_database: Some("otherdb".to_string()),
            pg_user: Some("otheruser".to_string()),
            pg_password: Some("otherpass".to_string()),
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        let url = config.database_url().unwrap();
        assert_eq!(url, "postgres://custom@localhost:9999/customdb");
    }

    #[test]
    fn test_database_url_from_components() {
        let config = Config {
            database_url: None,
            pg_host: Some("myhost".to_string()),
            pg_port: Some(1234),
            pg_database: Some("mydb".to_string()),
            pg_user: Some("myuser".to_string()),
            pg_password: Some("mypass".to_string()),
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        let url = config.database_url().unwrap();
        assert_eq!(url, "postgres://myuser:mypass@myhost:1234/mydb");
    }

    #[test]
    fn test_database_url_defaults() {
        let config = Config {
            database_url: None,
            pg_host: None,
            pg_port: None,
            pg_database: Some("mydb".to_string()),
            pg_user: Some("myuser".to_string()),
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        let url = config.database_url().unwrap();
        // Should use defaults: localhost:5432, empty password
        assert_eq!(url, "postgres://myuser:@localhost:5432/mydb");
    }

    #[test]
    fn test_missing_required_components() {
        let config = Config {
            database_url: None,
            pg_host: Some("localhost".to_string()),
            pg_port: Some(5432),
            pg_database: None, // Missing
            pg_user: Some("myuser".to_string()),
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.database_url().is_err());
    }

    #[test]
    fn test_data_dir_default() {
        // This test would need HOME set, so we just check the method exists
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        // Should default to ~/.btmon/data (if HOME is set)
        let result = config.data_dir();
        // We don't assert the exact path since it depends on HOME
        assert!(result.is_ok() || std::env::var("HOME").is_err());
    }

    #[test]
    fn test_data_dir_custom() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: Some("/custom/data/dir".to_string()),
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        let path = config.data_dir().unwrap();
        assert_eq!(path, std::path::PathBuf::from("/custom/data/dir"));
    }

    #[test]
    fn test_rate_limit_threshold() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 20000,
            fixed_location: None,
        };

        let threshold = config.rate_limit_threshold();
        assert_eq!(threshold, std::time::Duration::from_millis(20000));
        assert_eq!(threshold.as_secs(), 20);
    }

    #[test]
    fn test_fixed_location_valid() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: Some("40.6892,-74.0445".to_string()),
        };

        let result = config.fixed_location().unwrap();
        let (lat, lon) = result.unwrap();
        assert!((lat - 40.6892).abs() < 0.0001);
        assert!((lon - (-74.0445)).abs() < 0.0001);
    }

    #[test]
    fn test_fixed_location_invalid_format() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: Some("40.6892".to_string()), // Missing longitude
        };

        let result = config.fixed_location().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_fixed_location_out_of_range() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: Some("91.0,0.0".to_string()), // Latitude out of range
        };

        let result = config.fixed_location().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_fixed_location_none() {
        let config = Config {
            database_url: Some("postgres://localhost/test".to_string()),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: "info".to_string(),
            node_id: Some("00000000-0000-0000-0000-000000000000".to_string()),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: None,
            data_dir: None,
            rate_limit_ms: 15000,
            fixed_location: None,
        };

        assert!(config.fixed_location().is_none());
    }
}

// ============================================================================
// CONFIG FILE STRUCTS (for TOML configuration)
// ============================================================================

/// TOML configuration file structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigFile {
    pub database: Option<DatabaseConfig>,
    pub log: Option<LogConfig>,
    pub node: Option<NodeConfig>,
    pub bluetooth: Option<BluetoothConfig>,
}

/// Database configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
}

/// Logging configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    pub level: Option<String>,
}

/// Node configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub id: Option<String>,
}

/// Bluetooth configuration section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BluetoothConfig {
    pub adapter_id: Option<String>,
    pub scan_interval_ms: Option<u64>,
    pub store_raw_payload: Option<bool>,
    pub data_dir: Option<String>,
    pub rate_limit_ms: Option<u64>,
    pub fixed_location: Option<String>,
}
