//! Application configuration.

use clap::Parser;

/// Bluetooth Monitoring Application
///
/// Monitors Bluetooth Low Energy (BLE) devices and stores discoveries
/// in a PostgreSQL database.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Config {
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
}

impl Config {
    /// Parse configuration from CLI and environment.
    pub fn parse_args() -> Self {
        Self::parse()
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

        Ok(())
    }

    /// Get the parsed node_id as a UUID.
    pub fn node_id(&self) -> Result<uuid::Uuid, &'static str> {
        match &self.node_id {
            Some(s) => s.parse().map_err(|_| "Invalid UUID format"),
            None => Err("NODE_ID not set"),
        }
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
        };

        assert!(config.database_url().is_err());
    }
}
