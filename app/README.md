# Bluetooth Monitoring Application

This application monitors Bluetooth Low Energy (BLE) devices and stores discoveries in a PostgreSQL database.

## Features

- Real-time Bluetooth device scanning and monitoring
- Automatic storage of device discoveries in PostgreSQL
- Configurable scan intervals
- Multiple Bluetooth backends (btleplug cross-platform, bluer Linux-specific)
- Environment variable and CLI argument configuration
- Comprehensive logging with configurable levels

## Configuration

Configuration can be provided via environment variables or command-line arguments. CLI arguments take precedence over environment variables.

### Database Configuration

You can configure the database connection in **two ways**:

#### Option 1: Standard PostgreSQL Environment Variables (Recommended)

Uses the standard PostgreSQL environment variables:

| Variable | CLI Flag | Description |
|----------|----------|-------------|
| `PGHOST` | `--pg-host` | PostgreSQL host (default: `localhost`) |
| `PGPORT` | `--pg-port` | PostgreSQL port (default: `5432`) |
| `PGDATABASE` | `--pg-database` | Database name **(required if not using DATABASE_URL)** |
| `PGUSER` | `--pg-user` | Database user **(required if not using DATABASE_URL)** |
| `PGPASSWORD` | `--pg-password` | Database password |

#### Option 2: Connection String

Use a single connection string:

| Variable | CLI Flag | Description |
|----------|----------|-------------|
| `DATABASE_URL` | `-d, --database-url` | PostgreSQL connection string (overrides PG* vars) |

### Required Configuration

At minimum, you need:

| Variable | Description |
|----------|-------------|
| `NODE_ID` | UUID identifying this node (from certificate) |
| `PGDATABASE` + `PGUSER` | **OR** `DATABASE_URL` |

### Optional Configuration

| Variable | CLI Flag | Default | Description |
|----------|----------|---------|-------------|
| `LOG_LEVEL` | `--log-level` | `info` | Log level (debug, info, warn, error) |
| `BT_SCAN_INTERVAL_MS` | `--scan-interval-ms` | `1000` | Scan interval in milliseconds |
| `BT_STORE_RAW_PAYLOAD` | `--store-raw-payload` | `true` | Whether to store raw advertisement payload |
| `BT_ADAPTER_ID` | `--adapter-id` | (auto) | Specific Bluetooth adapter ID to use |

## Usage

### Environment Variable Setup (Using PG* vars)

```bash
# Set required environment variables
export PGHOST=localhost
export PGPORT=7789
export PGDATABASE=travel
export PGUSER=postgres
export PGPASSWORD=postgres

export NODE_ID="550e8400-e29b-41d4-a716-446655440000"

# Optional: Set log level
export LOG_LEVEL="debug"

# Run the application
cargo run --bin app
```

### Environment Variable Setup (Using DATABASE_URL)

```bash
# Set required environment variables
export DATABASE_URL="postgres://user:pass@localhost:7789/travel"
export NODE_ID="550e8400-e29b-41d4-a716-446655440000"

# Run the application
cargo run --bin app
```

### Command-Line Arguments

```bash
# Using PG* vars
cargo run --bin app -- \
  --pg-host localhost \
  --pg-port 7789 \
  --pg-database travel \
  --pg-user postgres \
  --pg-password pass \
  --node-id "550e8400-e29b-41d4-a716-446655440000" \
  --log-level debug

# Using DATABASE_URL
cargo run --bin app -- \
  -d "postgres://localhost:7789/travel" \
  --node-id "550e8400-e29b-41d4-a716-446655440000" \
  --log-level debug
```

### Help

```bash
cargo run --bin app -- --help
```

## Data Storage

The application stores Bluetooth device discoveries in the `bluetooth_occurrences` table. Each discovery record includes:

- **Device Information**: MAC address, advertised name, address type
- **Advertisement Details**: RSSI, advertisement type, service UUIDs, manufacturer data
- **Metadata**: Timestamp (both corrected and node-local), schema version
- **Location** (future): GPS coordinates if available

### Example Database Schema

```sql
CREATE TABLE bluetooth_occurrences (
    id UUID PRIMARY KEY,              -- UUIDv7, time-sortable
    node_id UUID NOT NULL,            -- Node that observed the device
    observed_at TIMESTAMPTZ NOT NULL, -- Clock-sync corrected timestamp
    observed_at_node_local TIMESTAMPTZ, -- Raw node timestamp
    device_address BYTEA NOT NULL,    -- 6-byte MAC address
    device_address_type TEXT,         -- public, random_static, etc.
    device_advertised_name TEXT,      -- Name from AD payload
    advertisement_type TEXT,          -- ADV_IND, SCAN_RSP, etc.
    rssi INTEGER,                     -- Signal strength in dBm
    service_uuids BYTEA[],            -- Array of 16-byte UUIDs
    manufacturer_company_id INTEGER,  -- Bluetooth SIG Company ID
    manufacturer_payload BYTEA,       -- Raw manufacturer data
    schema_version INTEGER NOT NULL,  -- Schema version for compatibility
    created_at TIMESTAMPTZ NOT NULL   -- Database insertion timestamp
);

CREATE INDEX idx_occurrences_observed_at ON bluetooth_occurrences(observed_at DESC);
CREATE INDEX idx_occurrences_node_id ON bluetooth_occurrences(node_id);
CREATE INDEX idx_occurrences_device_address ON bluetooth_occurrences(device_address);
```

## Bluetooth Backend Selection

By default, the application uses the `btleplug` backend (cross-platform). You can switch to the `bluer` backend (Linux-specific) by building with the appropriate features:

```bash
# Default (btleplug backend)
cargo run --bin app

# Linux-specific bluer backend
cargo run --bin app --features bluer --no-default-features
```

## Permissions

### Linux

- Bluetooth access typically requires root or membership in the `bluetooth` group
- Running as a non-root user: `sudo usermod -aG bluetooth $USER`

### macOS

- Bluetooth access may require Bluetooth permissions in System Preferences

## Logging

Log levels can be configured to control output verbosity:

- **debug**: Detailed debugging information, including individual events
- **info**: General operational information (default)
- **warn**: Warning messages for potentially problematic situations
- **error**: Error messages only

Example with debug logging:

```bash
LOG_LEVEL=debug cargo run --bin app
```

## Troubleshooting

### Bluetooth adapter not found

```
Error: Bluetooth error: Adapter not found
```

Ensure you have a Bluetooth adapter connected and it's powered on.

### Bluetooth adapter not powered

```
WARN: Bluetooth adapter is not powered on.
```

Power on your Bluetooth adapter through system settings or:

```bash
# Linux with bluetoothctl
bluetoothctl
[bluetooth]# power on
```

### Database connection failed

Ensure PostgreSQL is running and the connection string is correct. Test with:

```bash
psql "$DATABASE_URL"
```

### Permission denied

On Linux, run with appropriate permissions:

```bash
sudo cargo run --bin app
# or add your user to the bluetooth group
```

## Development

### Running Tests

```bash
cargo test
```

### Building for Release

```bash
cargo build --release
```

### Code Quality

```bash
cargo fmt        # Format code
cargo clippy     # Lint code
cargo test       # Run tests
```

## Architecture

```
app/
├── src/
│   ├── main.rs       # Entry point
│   ├── app.rs        # Application state and event loop
│   ├── config.rs     # Configuration parsing
│   └── error.rs      # Error types
└── Cargo.toml
```

### Event Flow

1. Application starts and initializes Bluetooth monitor
2. Scanning begins for nearby BLE devices
3. Device events are received from the monitor
4. Each `DeviceAdded` event is converted to a `BluetoothOccurrence`
5. Occurrence is inserted into the database
6. Process repeats for each discovered device

## Future Enhancements

- Graceful shutdown handling (SIGINT/SIGTERM)
- GPS integration for location data
- Raw advertisement payload capture
- Device address type detection
- Health/metrics endpoint
- Configuration file support (TOML/YAML)

## License

See the root LICENSE file.
