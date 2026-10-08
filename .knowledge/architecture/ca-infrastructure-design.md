# CA Infrastructure Architecture Decision

**Date:** 2026-08-26
**Decision:** CA infrastructure as **`--lib` crate** integrated into `app`

---

## Options Considered

### Option A: Separate Binary (`ca_server`)
**Structure:**
```
workspace/
├── app/              # Main application binary
├── ca_server/        # CA service binary (NEW)
├── bt_mon/           # BLE monitoring library
├── db/               # Database migrations
└── repo/             # Repository layer
```

**Trade-offs:**

✅ **Pros:**
- Clear separation of concerns (node vs. CA operator)
- Independent deployment/Scaling of CA service
- Can run CA on different hardware/infrastructure
- Easier to secure (isolated process, network boundaries)
- Multi-process security model (principle of least privilege)
- CA can serve multiple independent deployments
- Natural fit for step-ca integration (different binary anyway)

❌ **Cons:**
- Adds operational complexity (another service to manage)
- Network latency for enrollment/verification lookups
- Requires service discovery/configuration
- Additional failure domain (CA service downtime blocks enrollment)
- Need HTTP/gRPC API definition
- Certificate/key distribution becomes network operation
- Overkill for small deployments (single-digit nodes)

---

### Option B: Library Crate (`ca`) + Integrated into `app`
**Structure:**
```
workspace/
├── app/              # Main application binary (includes CA logic)
│   └── src/ca/       # CA module (NEW)
├── bt_mon/           # BLE monitoring library
├── db/               # Database migrations
├── repo/             # Repository layer
└── ca/               # Shared CA library (NEW, --lib)
```

**Trade-offs:**

✅ **Pros:**
- Simpler deployment (single binary)
- No network overhead for local operations
- Easier testing (in-memory CA, no service needed)
- Lower operational overhead for small deployments
- Direct database access (no serialization layer)
- Faster enrollment (local function call vs. HTTP)
- Can easily switch between "embedded CA" and "remote CA" modes
- Fits Phase 0/0.5 scope (single-node → small federation)

❌ **Cons:**
- Tightly couples CA to app lifecycle
- Harder to secure (same process, same privileges)
- Cannot scale CA independently
- All nodes need CA credentials baked in (or distributed separately)
- Migration path to external CA requires refactoring
- Larger attack surface (CA code runs on every node)

---

### Option C: Library Crate + CLI Subcommand
**Structure:**
```
workspace/
├── app/              # Main application binary
│   └── src/ca/       # CA module (NEW)
├── ca/               # Shared CA library (NEW, --lib)
├── bt_mon/           # BLE monitoring library
└── ...
```

**Usage:**
```bash
# Run node
./app run

# Run CA enrollment server (optional, for federation)
./app ca-server --listen 0.0.0.0:8443

# One-off enrollment
./app ca-enroll --node-id <id> --output credential.json
```

**Trade-offs:**

✅ **Pros:**
- Best of both worlds: embedded + optional server mode
- Progressive enhancement (start simple, scale later)
- Same codebase, different entry points
- No new binary to maintain
- CLI is natural for one-off operations (key rotation, revocation)
- Tests can use library directly
- Production can run server mode for fleet management

❌ **Cons:**
- Slightly more complex build (multiple entry points)
- Need to manage feature flags for server mode
- Still couples CA to app lifecycle in embedded mode
- Binary size increases (CA code always compiled in)

---

## Recommendation: **Option C (Library + CLI Subcommand)**

### Rationale

1. **Matches Current Architecture Style**
   - `db` crate is already a library with CLI tool
   - Follows same pattern: `--lib` crate + `app` CLI subcommand
   - Consistent with workspace organization

2. **Fits Development Phases**
   - **Phase 0/0.5:** Embedded CA (single node, self-enrollment)
   - **Phase 1-2:** Optional server mode (small federation)
   - **Phase 5-6:** Can migrate to step-ca (external service)

3. **Operational Flexibility**
   - Start simple: `./app run` (CA embedded, self-enroll)
   - Scale up: `./app ca-server` (central CA for fleet)
   - Debug easily: `./app ca-revoke --node-id <id>`

4. **Testing Strategy**
   - Unit tests use library directly
   - Integration tests can spawn CA server process
   - End-to-end tests use embedded mode for simplicity

5. **Security Progression**
   - Phase 0: Self-signed (no real CA)
   - Phase 0.5: Embedded CA with local root key
   - Phase 5+: External CA (step-ca/Vault) - requires refactoring anyway

### Proposed Structure

```
workspace/
├── app/
│   ├── Cargo.toml          # Add: ca = { path = "../ca" }
│   └── src/
│       ├── main.rs         # Add: ca-server, ca-enroll subcommands
│       ├── ca/             # NEW: Embedded CA implementation
│       │   ├── mod.rs
│       │   ├── enroll.rs   # Enrollment logic
│       │   ├── verify.rs   # Credential verification
│       │   ├── revoke.rs   # Revocation management
│       │   └── storage.rs  # Credential persistence
│       └── ...
├── ca/                     # NEW: Shared CA library
│   ├── Cargo.toml          # --lib crate
│   └── src/
│       ├── lib.rs          # Public API
│       ├── credential.rs   # CA credential types
│       ├── signing.rs      # CA signing logic
│       ├── root.rs         # Root key management
│       └── error.rs
├── bt_mon/
├── db/
├── repo/
└── ...
```

### Implementation Phases

#### Phase 1: Library Core (`ca/`)
```rust
// ca/src/lib.rs
pub struct CaRoot {
    private_key: Ed25519PrivateKey,
    public_key: Ed25519PublicKey,
}

impl CaRoot {
    pub fn generate() -> Self;
    pub fn load_from_env() -> Self;
    pub fn issue_credential(&self, public_key: &Ed25519PublicKey) -> Credential;
    pub fn verify_credential(&self, credential: &Credential) -> bool;
}

pub struct Credential {
    pub node_id: Vec<u8>,
    pub signing_public_key: Vec<u8>,
    pub ca_signature: Vec<u8>,
    pub issued_at: DateTime,
    pub expires_at: Option<DateTime>,
}
```

#### Phase 2: Embedded Mode (`app/src/ca/`)
```rust
// app/src/ca/mod.rs
pub struct EmbeddedCa {
    root: CaRoot,
    db_pool: Pool,
}

impl EmbeddedCa {
    pub fn new(root_key_path: PathBuf, db_pool: Pool) -> Self;
    pub fn enroll_node(&self, signing_public_key: &[u8]) -> Result<Credential>;
    pub fn verify_node_credential(&self, node_id: &[u8]) -> Result<bool>;
}
```

#### Phase 3: CLI Subcommands (`app/src/cli.rs`)
```rust
#[derive(Subcommand)]
enum CaCommands {
    /// Start CA enrollment server
    CaServer {
        #[arg(long, default_value = "0.0.0.0:8443")]
        listen: String,
    },
    /// Enroll a node manually
    CaEnroll {
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        public_key: String,
    },
    /// Revoke a node's credentials
    CaRevoke {
        #[arg(long)]
        node_id: String,
    },
}
```

#### Phase 4: Server Mode (Optional, Phase 1+)
```rust
// app/src/ca/server.rs
pub async fn run_ca_server(listen_addr: SocketAddr, ca: EmbeddedCa) -> Result<()> {
    // HTTP/HTTPS server with endpoints:
    // POST /enroll  - Submit public key, receive credential
    // GET /verify   - Verify credential for node_id
    // POST /revoke  - Revoke node credential
}
```

### Feature Flags

```toml
# app/Cargo.toml
[features]
default = ["ca-embedded"]
ca-embedded = []      # Embedded CA mode (default for Phase 0.5)
ca-server = []        # HTTP server mode (for Phase 1+)
```

```rust
// app/src/main.rs
#[cfg(feature = "ca-server")]
if matches!(cmd, CaCommands::CaServer { .. }) {
    run_ca_server().await?;
}
```

### Configuration

```toml
# config.toml
[ca]
# Path to CA root key (PKCS#8 PEM Ed25519 private key)
root_key_path = "/var/lib/btmon/ca/root_key.pem"

# Generate root key if not exists (first run only)
auto_generate = true

# Server mode (optional, Phase 1+)
[ca.server]
enabled = false
listen_addr = "0.0.0.0:8443"
tls_cert = "/var/lib/btmon/ca/server.crt"
tls_key = "/var/lib/btmon/ca/server.key"
```

### Security Progression

| Phase | CA Mode | Root Key Storage | Access Control |
|-------|---------|------------------|----------------|
| 0.5 | Embedded | File (0600) | Local only |
| 1-2 | Embedded or Server | File (0600) | Network + TLS |
| 5+ | External (step-ca) | HSM/Secrets Manager | mTLS + RBAC |

### Migration Path to External CA

When migrating to step-ca (Phase 5+):

1. **Keep `ca/` library** - Interface stays same
2. **Replace implementation** - `CaRoot` now calls step-ca CLI/API
3. **Update config** - `ca.endpoint = "https://ca.example.com:9000"`
4. **Deploy** - No changes to `app/` code needed

```rust
// ca/src/lib.rs - External CA implementation
pub struct CaRoot {
    endpoint: Url,
    ca_cert: X509Certificate,
}

impl CaRoot {
    pub fn issue_credential(&self, public_key: &Ed25519PublicKey) -> Credential {
        // Call step-ca API instead of local signing
        http_post(&self.endpoint, CredentialRequest { public_key })
    }
}
```

---

## Why NOT Separate Binary (`ca_server/`)

1. **Over-engineering for current scope**
   - Phase 0.5 is single-node → small federation
   - No operational need for independent scaling
   - Can always split later (extract `ca/` into separate service)

2. **Added complexity without benefit**
   - Network serialization (HTTP/gRPC)
   - Service discovery
   - Health checking
   - Load balancing (if scaling)
   - None needed for Phase 0.5

3. **Testing becomes harder**
   - Need to spawn HTTP server in tests
   - Network timeouts, retries
   - TLS setup for tests
   - Slower test execution

4. **Step-ca integration is external anyway**
   - If we use step-ca (Phase 5+), it's already a separate binary
   - No need to build our own separate binary now
   - Can wrap step-ca in `ca/` library interface

---

## Conclusion

**Decision:** `ca/` as `--lib` crate integrated into `app`

**Rationale:**
- Matches existing workspace patterns (`db/` crate)
- Progressive enhancement (embedded → server → external)
- Simpler testing and deployment for Phase 0.5
- Migration path to external CA preserved
- Operational flexibility without premature complexity

**When to reconsider:**
- When deploying to 10+ nodes with separate operators
- When CA needs independent scaling (high enrollment volume)
- When security policy requires process isolation
- When migrating to step-ca (then use external CA directly)
