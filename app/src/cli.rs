//! CLI subcommands for the Bluetooth monitoring application.

use clap::Subcommand;
use repo::SignalType;
use sqlx::postgres::PgPoolOptions;

use crate::config::Config;

type Pool = sqlx::Pool<sqlx::Postgres>;

/// CLI subcommands
#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Start Bluetooth monitoring (default)
    Monitor {
        /// Node UUID for identification
        #[arg(long, env = "NODE_ID")]
        node_id: Option<String>,
    },

    /// Query occurrences from the database
    Query {
        /// Query by time range (e.g., "1h" for last hour, "30m" for last 30 minutes)
        #[arg(long)]
        last: Option<String>,

        /// Query by H3 geo cell (macro resolution)
        #[arg(long)]
        geo_cell: Option<u64>,

        /// Query by signal type
        #[arg(long, default_value = "bluetooth")]
        signal_type: String,

        /// Maximum number of results
        #[arg(long, default_value = "100")]
        limit: i64,

        /// Output format (text, json)
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Show node and database statistics
    Stats {
        /// Database URL
        #[arg(short, long, env = "DATABASE_URL")]
        database_url: Option<String>,
    },
}

/// Command-line application runner
pub struct Cli {
    pub command: Commands,
    pub config: Config,
    pub database_url: Option<String>,
    pub log_level: String,
    pub config_file: Option<String>,
}

impl Cli {
    /// Parse command-line arguments
    pub fn parse() -> Self {
        use clap::Parser;

        #[derive(Parser, Debug)]
        #[command(author, version, about, long_about = None)]
        struct Args {
            /// Configuration file (TOML format)
            #[arg(long, env = "CONFIG_FILE")]
            config_file: Option<String>,

            /// Database URL
            #[arg(short, long, env = "DATABASE_URL")]
            database_url: Option<String>,

            /// Log level (debug, info, warn, error)
            #[arg(long, env = "LOG_LEVEL", default_value = "info")]
            log_level: String,

            /// Node UUID for identification
            #[arg(long, env = "NODE_ID")]
            node_id: Option<String>,

            /// Bluetooth adapter ID
            #[arg(long, env = "BT_ADAPTER_ID")]
            adapter_id: Option<String>,

            /// Data directory for node identity
            #[arg(long, env = "BT_DATA_DIR")]
            data_dir: Option<String>,

            /// Rate limit threshold in milliseconds
            #[arg(long, env = "BT_RATE_LIMIT_MS", default_value = "15000")]
            rate_limit_ms: u64,

            /// Fixed location (lat,lon)
            #[arg(long, env = "BT_FIXED_LOCATION")]
            fixed_location: Option<String>,

            #[command(subcommand)]
            command: Option<Commands>,
        }

        let args = Args::parse();

        // Build base config from CLI args
        let config = Config {
            config_file: args.config_file.clone(),
            database_url: args.database_url.clone(),
            pg_host: None,
            pg_port: None,
            pg_database: None,
            pg_user: None,
            pg_password: None,
            log_level: args.log_level.clone(),
            node_id: args.node_id.clone(),
            scan_interval_ms: 1000,
            store_raw_payload: true,
            adapter_id: args.adapter_id.clone(),
            data_dir: args.data_dir.clone(),
            rate_limit_ms: args.rate_limit_ms,
            fixed_location: args.fixed_location.clone(),
        };

        // Default to "monitor" if no subcommand specified
        let command = args.command.unwrap_or(Commands::Monitor {
            node_id: args.node_id,
        });

        Self {
            command,
            config,
            database_url: args.database_url,
            log_level: args.log_level,
            config_file: args.config_file,
        }
    }

    /// Run the selected command
    pub async fn run(self) -> Result<(), String> {
        match self.command {
            Commands::Monitor { node_id } => {
                // This would call the FullNode monitor
                // For now, just print a message
                println!("Monitor command: node_id={:?}", node_id);
                Ok(())
            }

            Commands::Query {
                ref last,
                geo_cell,
                ref signal_type,
                limit,
                ref format,
            } => {
                let db_url = self.database_url.as_deref()
                    .ok_or("DATABASE_URL required for query command")?;
                self.run_query(db_url, last.clone(), geo_cell, signal_type.clone(), limit, format).await
            }

            Commands::Stats { ref database_url } => {
                let db_url_opt = self.database_url.clone();
                let url = database_url.clone().or(db_url_opt)
                    .ok_or("DATABASE_URL required for stats command")?;
                self.run_stats(&url).await
            }
        }
    }

    /// Run query command
    async fn run_query(
        &self,
        url: &str,
        last: Option<String>,
        geo_cell: Option<u64>,
        signal_type: String,
        limit: i64,
        format: &str,
    ) -> Result<(), String> {

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // Parse signal type
        let signal_type = match signal_type.to_lowercase().as_str() {
            "bluetooth" | "ble" => SignalType::Bluetooth,
            "wifi" => SignalType::Wifi,
            "nfc" => SignalType::Nfc,
            "zigbee" => SignalType::Zigbee,
            _ => return Err(format!("Unknown signal type: {}", signal_type)),
        };

        // Build query based on parameters
        let occurrences = if let Some(ref last_str) = last {
            // Parse time duration (e.g., "1h", "30m", "7d")
            let duration = parse_duration(last_str)
                .map_err(|e| format!("Invalid duration '{}': {}", last_str, e))?;
            let since = chrono::Utc::now() - duration;

            query_by_time(&pool, signal_type, since, limit).await?
        } else if let Some(cell) = geo_cell {
            query_by_geo_cell(&pool, cell as i64, limit).await?
        } else {
            // Default: query by signal type
            query_by_signal_type(&pool, signal_type, limit).await?
        };

        // Output results
        match format.to_lowercase().as_str() {
            "json" => {
                println!("{}", serde_json::to_string_pretty(&occurrences)
                    .map_err(|e| format!("Failed to serialize: {}", e))?);
            }
            _ => {
                println!("Found {} occurrences:", occurrences.len());
                for occ in &occurrences {
                    println!(
                        "  {} - {:?} - RSSI: {}dBm - {}",
                        occ.occurrence_id,
                        occ.signal_type,
                        occ.rssi,
                        occ.observed_at
                    );
                }
            }
        }

        Ok(())
    }

    /// Run stats command
    async fn run_stats(&self, url: &str) -> Result<(), String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        // Count total occurrences
        let total_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM occurrences"
        )
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("Failed to count occurrences: {}", e))?;

        // Count by signal type
        let signal_counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT signal_type, COUNT(*) FROM occurrences GROUP BY signal_type ORDER BY signal_type"
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| format!("Failed to count by signal type: {}", e))?;

        // Count nodes
        let node_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nodes"
        )
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("Failed to count nodes: {}", e))?;

        // Print stats
        println!("=== Database Statistics ===");
        println!("Total occurrences: {}", total_count);
        println!("Total nodes: {}", node_count);
        println!("\nOccurrences by signal type:");
        for (signal_type, count) in &signal_counts {
            println!("  {}: {}", signal_type, count);
        }

        Ok(())
    }
}

/// Parse a duration string (e.g., "1h", "30m", "7d")
fn parse_duration(s: &str) -> Result<chrono::Duration, String> {
    use chrono::Duration;

    let s = s.trim().to_lowercase();
    let mut chars = s.chars().peekable();

    let mut num = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            num.push(chars.next().unwrap());
        } else {
            break;
        }
    }

    let unit: String = chars.collect();
    let num: i64 = num.parse().map_err(|_| "Invalid number")?;

    match unit.as_str() {
        "s" | "sec" | "second" | "seconds" => Ok(Duration::seconds(num)),
        "m" | "min" | "minute" | "minutes" => Ok(Duration::minutes(num)),
        "h" | "hour" | "hours" => Ok(Duration::hours(num)),
        "d" | "day" | "days" => Ok(Duration::days(num)),
        _ => Err(format!("Unknown time unit: '{}'", unit)),
    }
}

/// Query occurrences by time range
async fn query_by_time(
    pool: &Pool,
    signal_type: SignalType,
    since: chrono::DateTime<chrono::Utc>,
    limit: i64,
) -> Result<Vec<repo::models::occurrence::Occurrence>, String> {
    let results = sqlx::query_as::<_, repo::models::occurrence::Occurrence>(
        "SELECT * FROM occurrences WHERE signal_type = $1 AND observed_at >= $2 ORDER BY observed_at DESC LIMIT $3"
    )
    .bind(signal_type)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Query failed: {}", e))?;

    Ok(results)
}

/// Query occurrences by geo cell
async fn query_by_geo_cell(
    pool: &Pool,
    geo_cell: i64,
    limit: i64,
) -> Result<Vec<repo::models::occurrence::Occurrence>, String> {
    let results = sqlx::query_as::<_, repo::models::occurrence::Occurrence>(
        "SELECT * FROM occurrences WHERE geo_cell_macro = $1 ORDER BY observed_at DESC LIMIT $2"
    )
    .bind(geo_cell)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Query failed: {}", e))?;

    Ok(results)
}

/// Query occurrences by signal type
async fn query_by_signal_type(
    pool: &Pool,
    signal_type: SignalType,
    limit: i64,
) -> Result<Vec<repo::models::occurrence::Occurrence>, String> {
    let results = sqlx::query_as::<_, repo::models::occurrence::Occurrence>(
        "SELECT * FROM occurrences WHERE signal_type = $1 ORDER BY observed_at DESC LIMIT $2"
    )
    .bind(signal_type)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Query failed: {}", e))?;

    Ok(results)
}
