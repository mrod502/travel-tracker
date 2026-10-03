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
| `PGDATABASE` + `PGUSER` | **OR** `DATABASE_URL` |

Nothing else is required. The node's identity comes from the Ed25519 key in the
data directory (`BT_DATA_DIR`, default `~/.btmon/data`), which is created on first
run; `NODE_ID` states which identity the node has to *turn out* to be and is
optional — see below.

### Optional Configuration

| Variable | CLI Flag | Default | Description |
|----------|----------|---------|-------------|
| `LOG_LEVEL` | `--log-level` | `info` | Log level (debug, info, warn, error) |
| `NODE_ID` | `--node-id` | (unset) | The node id this node has to turn out to be: 64 hex characters, SHA-256 of its signing key. Checked against the key file, never used as an identity; a mismatch stops startup and prints both ids. A UUID here is an error — that is what this setting asked for before |
| `BT_SCAN_INTERVAL_MS` | `--scan-interval-ms` | `1000` | How often the continuous scan is re-armed, and how often the simulated radio advertises. Not a duty cycle |
| `BT_STORE_RAW_PAYLOAD` | `--store-raw-payload` | `true` | Keep the radio's advertisement bytes as `signal_payload.ble.raw_payload_hex`, inside the signed payload. A backend that exposes only decoded properties stores nothing extra and says so once |
| `BT_ADAPTER_ID` | `--adapter-id` | (first adapter) | Which radio to open, by id, name, or MAC address. One that matches nothing — or more than one adapter — stops startup instead of picking |

## Usage

### Environment Variable Setup (Using PG* vars)

```bash
# Set required environment variables
export PGHOST=localhost
export PGPORT=7789
export PGDATABASE=travel
export PGUSER=postgres
export PGPASSWORD=postgres

# Optional: assert the identity this node must have, as the startup log prints it
# export NODE_ID="0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0"

# Optional: Set log level
export LOG_LEVEL="debug"

# Run the application
cargo run --bin app
```

### Environment Variable Setup (Using DATABASE_URL)

```bash
# Set required environment variables
export DATABASE_URL="postgres://user:pass@localhost:7789/travel"

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
  --log-level debug

# Using DATABASE_URL, on a machine with two radios, asserting its enrolled identity
cargo run --bin app -- \
  -d "postgres://localhost:7789/travel" \
  --adapter-id hci1 \
  --node-id "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0" \
  --log-level debug
```

### Help

```bash
cargo run --bin app -- --help
```

## Derived Device Identity

`occurrences` record advertisements and never ask whether two of them came from the same device.
The batch that asks is `identity-replay`:

```bash
cargo run --bin app -- identity-replay --last 7d              # read, print, write nothing
cargo run --bin app -- identity-replay --last 7d --write      # and persist the derived tables
cargo run --bin app -- identity-replay --last 7d --json       # same pass, machine-readable
cargo run --bin app -- identity-replay --last 1d --node <64-hex>  # one node's rows only
```

It reads a window of stored occurrences oldest-first, replays them through `bt_iden`'s resolver, and
prints every decision with the features that carried it, the score, and the weight that was *not*
observed:

```text
== device identity replay: 55 observations, 10 identities, 45 merges, mean coverage 0.36 ==
   1 2026-09-23T00:46:26 f0ee00000000 ble_mac  -> #1   new                   cov   -   score      -
       not observed: uuid_overlap 30, appearance 15
   2 2026-09-23T00:46:26 f0ee00000001 ble_mac  -> #2   contradicted          cov 0.76 score  109.1
       carried: manufacturer_id 40, time_continuity 25, payload_similarity 17, field_layout 15
       vetoed by a directly-observed name
       not observed: uuid_overlap 30, appearance 15
```

Which is what the report is for: `#2` scores 109.1, well past the merge threshold, on a manufacturer ID
shared by every beacon in the batch and a payload that differs by one byte — and it is refused anyway,
because it advertised a different name. The score line and the veto line are printed together so the
near-miss is visible, not just the answer.

Mean coverage is the number to read first: it is how much of the 185-point scoring model the stored
rows can actually feed, and on a capture that stored no advertisement bytes it is low because the
features genuinely are not there — the run says so instead of scoring their absence as disagreement.

`--write` populates four derived tables — `device_identities`, `device_address_links`,
`co_occurrence_events`, `association_edges` — and nothing else. Rows are keyed by a feature
fingerprint rather than by any identifier, so re-running a window converges on the rows the previous
run wrote instead of appending a second opinion; `occurrences` themselves are never touched, because
those rows are signed assertions by a node. `--min-evidence-ratio` and `--merge-threshold` tune the
resolver for the pass (`0.35` and `40` by default).

The batch is deliberately not wired into `monitor`: merges need to be reviewed before anything
downstream depends on them. The first pass over live data produced exactly the finding the review step
exists for — five identically-provisioned mock beacons merged into a single identity by the run whose
rows carried advertisement structures — and that finding is now closed in `bt_iden`, where an advertised
name that is neither the identity's name nor a truncation of it is one of the four Direct-quality vetoes
that refuse a merge. The same window resolves five identities where it resolved one. What remains for
review is the other half of the same data: the batch that stored no advertisement bytes resolves the same
five beacons at coverage 0.32 — carried by exact address match, name, time continuity and RSSI — so each
beacon is currently two rows reachable through `device_address_links` rather than one. See [B12 and B13](../GAP_ANALYSIS.md#81-blocking).

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
