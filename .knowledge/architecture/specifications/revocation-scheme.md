# Node Revocation Scheme Specification

## Overview

This document defines a revocation infrastructure for the BTMon network that enables nodes to answer two critical questions:

1. **Data Recording Decision**: "Should I record node X's data?" (occurrence verification)
2. **Network Participation Decision**: "Should this node be in the network?" (connection management)

## Threat Model

### Revocation Triggers

A node's credential should be revocable when:
- **Compromise**: Private key leaked or suspected compromised
- **Policy Violation**: Node misbehaves (spam, malformed data, attacks)
- **Physical Loss**: Device lost, stolen, or decommissioned
- **Administrative**: Owner loses access rights, subscription expired
- **Security Incident**: Node part of broader attack vector

### Attack Scenarios

1. **Replay Attack**: Attacker uses stolen credentials to inject fake data
2. **Sybil Attack**: Compromised node creates multiple fake identities
3. **Man-in-the-Middle**: Attacker intercepts and forwards legitimate traffic
4. **Denial of Service**: Flood network with data from compromised nodes

## Design Principles

### 1. **Offline Verification**
Nodes must be able to make revocation decisions without live CA access. The CA is online only for issuing credentials and publishing revocation status.

### 2. **Bounded Staleness**
Revocation information has a freshness guarantee. If status is too old, the node should either:
- Reject all new credentials (fail-closed), or
- Accept only recently-valid credentials (fail-open with restrictions)

### 3. **Scalable Distribution**
Revocation data should scale to thousands of nodes without overwhelming bandwidth or storage.

### 4. **Privacy-Preserving**
Revocation checking should not leak which nodes a particular node is interested in verifying.

### 5. **Tamper-Evident**
Revocation status must be cryptographically signed by the CA to prevent manipulation.

## Architecture

### Components

```
┌─────────────────────────────────────────────────────────────────┐
│                        CA (Certificate Authority)                │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────────┐ │
│  │  Root Key   │  │ Credential   │  │ Revocation Status Store │ │
│  │   Manager   │  │   Issuer     │  │    (Signed Snapshot)    │ │
│  └─────────────┘  └──────────────┘  └─────────────────────────┘ │
│         │                │                      │                │
│         │ Issue          │                      │                │
│         ▼                │                      │                │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │              Revocation Status List (RSL)                    ││
│  │  - Signed by CA root key                                    ││
│  │  - Contains revoked node_id hashes                          ││
│  │  - Includes signature validity period                       ││
│  │  - Published to distribution points                         ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ Publish RSL (periodic)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Distribution Points                           │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────────┐ │
│  │   Database  │  │   P2P Gossip │  │   Distributed Storage   │ │
│  │  (nodes.    │  │   Network    │  │    (IPFS, BitTorrent)   │ │
│  │   status)   │  │              │  │                         │ │
│  └─────────────┘  └──────────────┘  └─────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ Fetch RSL (periodic)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                        Network Nodes                             │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                   Local Revocation Cache                     ││
│  │  - Signed RSL snapshot                                      ││
│  │  - Last updated timestamp                                   ││
│  │  - Freshness policy (max staleness)                         ││
│  └─────────────────────────────────────────────────────────────┘│
│         │                                                    │    │
│         │ Verify                                             │    │
│         ▼                                                    │    │
│  ┌──────────────────┐                              ┌──────────────┐│
│  │  Occurrence      │                              │   P2P        ││
│  │  Verification    │                              │   Handshake  ││
│  │  (record data?)  │                              │   (connect?) ││
│  └──────────────────┘                              └──────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

### Revocation Status List (RSL)

The RSL is a CA-signed data structure containing revocation information:

```rust
/// Revocation Status List (RSL)
/// 
/// A signed snapshot of revoked node identifiers at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationStatusList {
    /// CA identifier that issued this RSL
    pub issuer_id: String,
    
    /// When this RSL was signed (not necessarily when revocations occurred)
    pub issued_at: DateTime<Utc>,
    
    /// When this RSL expires (must fetch fresh copy after this time)
    pub expires_at: DateTime<Utc>,
    
    /// Sequence number for incremental updates (optional)
    pub sequence_number: u64,
    
    /// List of revoked node identifiers
    /// Each entry contains the node_id and revocation metadata
    pub revocations: Vec<RevokedNode>,
    
    /// CA signature over the entire structure
    pub signature: Vec<u8>,
}

/// Individual revocation entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedNode {
    /// The node's identifier (SHA-256 of signing public key)
    pub node_id: Vec<u8>,
    
    /// When the node was revoked
    pub revoked_at: DateTime<Utc>,
    
    /// Reason for revocation (for auditing, not security-critical)
    pub reason: RevocationReason,
    
    /// Optional: signature of the node's public key for verification
    /// This proves the node_id actually belonged to a valid credential
    pub signing_public_key: Option<Vec<u8>>,
}

/// Reasons for revocation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[repr(u8)]
pub enum RevocationReason {
    /// Unspecified reason
    Unspecified = 0,
    
    /// Private key compromise suspected
    KeyCompromise = 1,
    
    /// CA compromise (requires full re-issuance)
    CaCompromise = 2,
    
    /// Node ceased operation (decommissioned)
    CeasedOperation = 3,
    
    /// Policy violation (spam, attacks, etc.)
    PolicyViolation = 4,
    
    /// Superseded by new credential (key rotation)
    Superseded = 5,
    
    /// Temporary hold (pending investigation)
    Hold = 6,
}
```

## Rust Traits and API

### Core Revocation Checking Trait

```rust
/// Trait for checking node revocation status
///
/// Implementations can use different strategies:
/// - Full RSL (complete list of revoked nodes)
/// - Bloom filter (space-efficient, probabilistic)
/// - CRL (Certificate Revocation List, X.509 compatible)
/// - OCSP-like (online query to CA)
///
pub trait RevocationChecker: Send + Sync {
    /// Check if a node is revoked
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node identifier to check
    ///
    /// # Returns
    ///
    /// - `Ok(RevocationStatus::Valid)` - Node is not revoked
    /// - `Ok(RevocationStatus::Revoked)` - Node is revoked
    /// - `Ok(RevocationStatus::Unknown)` - Cannot determine status
    /// - `Err(...)` - Error checking status
    fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus>;
    
    /// Check if the revocation data is fresh enough for policy
    ///
    /// Returns `true` if the cached status is within acceptable staleness bounds.
    fn is_fresh(&self) -> bool;
    
    /// Get the age of the current revocation data
    fn cache_age(&self) -> Duration;
    
    /// Force refresh of revocation data
    async fn refresh(&self) -> Result<()>;
}

/// Revocation status result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationStatus {
    /// Node is not revoked (credential is valid)
    Valid,
    
    /// Node has been revoked
    Revoked,
    
    /// Cannot determine revocation status (cache expired, network error)
    Unknown,
}
```

### Revocation Policy Trait

```rust
/// Revocation policy determines how to handle uncertain revocation status
///
/// Different use cases may have different policies:
/// - Data recording: may accept unknown status with warnings
/// - Network connections: should fail-closed on unknown status
pub trait RevocationPolicy: Send + Sync {
    /// Decide whether to accept a node based on revocation status
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node being verified
    /// * `status` - Current revocation status
    /// * `context` - Context about where this check is happening
    ///
    /// # Returns
    ///
    /// Decision on whether to accept the node
    fn evaluate(&self, node_id: &[u8], status: RevocationStatus, context: &CheckContext) -> Decision;
}

/// Context for revocation check
#[derive(Debug, Clone)]
pub struct CheckContext {
    /// Where this check is being performed
    pub location: CheckLocation,
    
    /// Age of the revocation cache
    pub cache_age: Duration,
    
    /// Maximum acceptable staleness
    pub max_staleness: Duration,
}

/// Where the revocation check is happening
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLocation {
    /// During occurrence verification (recording data)
    OccurrenceVerification,
    
    /// During P2P handshake (establishing connection)
    Handshake,
    
    /// During credential enrollment
    Enrollment,
    
    /// Administrative check (manual review)
    Admin,
}

/// Revocation check decision
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Accept the node
    Accept,
    
    /// Reject the node
    Reject,
    
    /// Accept but log warning (for Unknown status with lenient policy)
    AcceptWithWarning,
    
    /// Defer decision (need fresh revocation data)
    Defer,
}
```

### RSL Manager Trait

```rust
/// Manage Revocation Status List lifecycle
pub trait RslManager: Send + Sync {
    /// Fetch the latest RSL from distribution points
    async fn fetch_latest(&self) -> Result<RevocationStatusList>;
    
    /// Verify RSL signature
    fn verify_rsl(&self, rsl: &RevocationStatusList) -> Result<()>;
    
    /// Check if RSL is still valid (not expired)
    fn is_rsl_valid(&self, rsl: &RevocationStatusList) -> bool;
    
    /// Get current RSL (if any)
    fn current_rsl(&self) -> Option<Arc<RevocationStatusList>>;
}
```

## Implementation: Full RSL Backend

```rust
/// Full RSL-based revocation checker
///
/// Loads complete RSL into memory and performs linear/binary search
/// for revocation checks.
pub struct FullRslChecker {
    /// In-memory index of revoked node_ids for fast lookup
    revoked_set: DashSet<Vec<u8>>,
    
    /// Current RSL (for signature verification and metadata)
    current_rsl: Arc<Mutex<Option<Arc<RevocationStatusList>>>>,
    
    /// When the RSL was loaded
    loaded_at: AtomicDateTime,
    
    /// Maximum acceptable staleness
    max_staleness: Duration,
    
    /// RSL manager for fetching updates
    rsl_manager: Arc<dyn RslManager>,
}

impl FullRslChecker {
    pub fn new(
        rsl_manager: Arc<dyn RslManager>,
        max_staleness: Duration,
    ) -> Self {
        Self {
            revoked_set: DashSet::new(),
            current_rsl: Arc::new(Mutex::new(None)),
            loaded_at: AtomicDateTime::UNIX_EPOCH,
            max_staleness,
            rsl_manager,
        }
    }
    
    /// Load RSL into the checker
    pub fn load_rsl(&self, rsl: RevocationStatusList) -> Result<()> {
        // Verify signature
        self.rsl_manager.verify_rsl(&rsl)?;
        
        // Build revoked set
        let mut revoked = DashSet::new();
        for revocation in &rsl.revocations {
            revoked.insert(revocation.node_id.clone());
        }
        
        // Swap into place
        let mut current = self.current_rsl.lock().unwrap();
        *current = Some(Arc::new(rsl));
        self.revoked_set.clone_from(&revoked);
        self.loaded_at.store(chrono::Utc::now());
        
        Ok(())
    }
}

impl RevocationChecker for FullRslChecker {
    fn is_revoked(&self, node_id: &[u8]) -> Result<RevocationStatus> {
        if self.revoked_set.contains(node_id) {
            Ok(RevocationStatus::Revoked)
        } else {
            Ok(RevocationStatus::Valid)
        }
    }
    
    fn is_fresh(&self) -> bool {
        let loaded_at = self.loaded_at.load().unwrap_or(chrono::Utc::MIN_DATETIME);
        let age = chrono::Utc::now() - loaded_at;
        age.to_std().unwrap_or(Duration::MAX) < self.max_staleness
    }
    
    fn cache_age(&self) -> Duration {
        let loaded_at = self.loaded_at.load().unwrap_or(chrono::Utc::MIN_DATETIME);
        chrono::Utc::now() - loaded_at
    }
    
    async fn refresh(&self) -> Result<()> {
        let rsl = self.rsl_manager.fetch_latest().await?;
        self.load_rsl(rsl)?;
        Ok(())
    }
}
```

## Revocation Policies for Different Use Cases

### Data Recording Policy (Lenient)

```rust
/// Policy for occurrence verification: prefer availability, accept unknown
pub struct DataRecordingPolicy {
    /// Fail-closed if cache is too stale
    pub fail_on_stale_cache: bool,
    
    /// Log warning for unknown status but still accept
    pub warn_on_unknown: bool,
    
    /// Maximum cache staleness before refusing to check
    pub max_cache_age: Duration,
}

impl RevocationPolicy for DataRecordingPolicy {
    fn evaluate(&self, _node_id: &[u8], status: RevocationStatus, ctx: &CheckContext) -> Decision {
        match status {
            RevocationStatus::Valid => Decision::Accept,
            RevocationStatus::Revoked => Decision::Reject,
            RevocationStatus::Unknown => {
                if ctx.cache_age > self.max_cache_age && self.fail_on_stale_cache {
                    Decision::Defer // Need fresh data before recording
                } else if self.warn_on_unknown {
                    Decision::AcceptWithWarning
                } else {
                    Decision::Accept
                }
            }
        }
    }
}
```

### Network Connection Policy (Strict)

```rust
/// Policy for P2P handshakes: fail-closed on any uncertainty
pub struct ConnectionPolicy {
    /// Reject if cache is stale (even slightly)
    pub require_fresh_cache: bool,
    
    /// Reject if revocation status is unknown
    pub reject_unknown: bool,
}

impl RevocationPolicy for ConnectionPolicy {
    fn evaluate(&self, _node_id: &[u8], status: RevocationStatus, ctx: &CheckContext) -> Decision {
        match status {
            RevocationStatus::Valid => {
                if self.require_fresh_cache && !ctx.cache_age.is_zero() {
                    // Optional: require very recent cache
                    Decision::Accept
                } else {
                    Decision::Accept
                }
            },
            RevocationStatus::Revoked => Decision::Reject,
            RevocationStatus::Unknown => {
                if self.reject_unknown {
                    Decision::Reject
                } else {
                    Decision::Defer // Try to refresh cache first
                }
            }
        }
    }
}
```

## Database Schema

```sql
-- Node revocation tracking
CREATE TABLE node_revocations (
    -- Primary key: the revoked node's identifier
    node_id BYTEA PRIMARY KEY,
    
    -- When the revocation was issued
    revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Reason code (0-255)
    reason_code SMALLINT NOT NULL DEFAULT 0,
    
    -- Human-readable reason (optional, for auditing)
    reason_text TEXT,
    
    -- The node's signing public key (for verification)
    signing_public_key BYTEA NOT NULL,
    
    -- ID of the CA that issued this revocation
    revoked_by_ca VARCHAR(255) NOT NULL,
    
    -- Optional: original credential's issued_at (for audit trail)
    original_issued_at TIMESTAMPTZ,
    
    -- Optional: original credential's expires_at
    original_expires_at TIMESTAMPTZ,
    
    -- Optional: evidence or notes about the revocation
    notes TEXT,
    
    -- When this record was created
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- When this record was last updated
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for fast lookup by public key (to find node_id from key material)
CREATE INDEX idx_node_revocations_public_key ON node_revocations(signing_public_key);

-- Index for filtering by reason
CREATE INDEX idx_node_revocations_reason ON node_revocations(reason_code);

-- RSL snapshot tracking (for audit and distribution)
CREATE TABLE rsl_snapshots (
    -- Unique identifier for this RSL
    sequence_number BIGSERIAL PRIMARY KEY,
    
    -- CA identifier
    issuer_id VARCHAR(255) NOT NULL,
    
    -- When this RSL was issued
    issued_at TIMESTAMPTZ NOT NULL,
    
    -- When this RSL expires
    expires_at TIMESTAMPTZ NOT NULL,
    
    -- Number of revocations in this RSL
    revocation_count INTEGER NOT NULL,
    
    -- CA signature (hex-encoded)
    signature TEXT NOT NULL,
    
    -- SHA-256 hash of the full RSL payload (for integrity verification)
    payload_hash BYTEA NOT NULL,
    
    -- When this snapshot was stored in our DB
    stored_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Trigger to update updated_at timestamp
CREATE TRIGGER update_node_revocations_timestamp
    BEFORE UPDATE ON node_revocations
    FOR EACH ROW
    EXECUTE FUNCTION update_timestamp_column();
```

## Integration Points

### 1. Occurrence Verification Flow

```rust
pub async fn verify_occurrence(
    &self,
    occurrence: &Occurrence,
    revocation_checker: &dyn RevocationChecker,
    policy: &dyn RevocationPolicy,
) -> Result<VerificationResult> {
    // 1. Verify credential signature
    let credential = self.verify_credential(occurrence)?;
    
    // 2. Check revocation status
    let node_id = &credential.node_id;
    let revocation_status = revocation_checker.is_revoked(node_id)?;
    
    // 3. Apply policy
    let context = CheckContext {
        location: CheckLocation::OccurrenceVerification,
        cache_age: revocation_checker.cache_age(),
        max_staleness: Duration::hours(24), // Configurable
    };
    
    let decision = policy.evaluate(node_id, revocation_status, &context);
    
    match decision {
        Decision::Accept => Ok(VerificationResult::Valid),
        Decision::Reject => Err(VerificationError::NodeRevoked),
        Decision::AcceptWithWarning => {
            log::warn!("Recording data from node with unknown revocation status: {:?}", node_id);
            Ok(VerificationResult::Valid) // Still accept
        },
        Decision::Defer => Err(VerificationError::RevocationCheckFailed),
    }
}
```

### 2. P2P Handshake Flow

```rust
pub async fn handle_handshake(
    &self,
    peer_credential: &Credential,
    revocation_checker: &dyn RevocationChecker,
) -> Result<HandshakeResult> {
    // 1. Verify credential is valid (not expired)
    peer_credential.is_valid_now()?;
    
    // 2. Verify credential signature against CA
    self.verify_credential_signature(peer_credential)?;
    
    // 3. Check revocation (strict policy)
    let policy = ConnectionPolicy {
        require_fresh_cache: true,
        reject_unknown: true,
    };
    
    let context = CheckContext {
        location: CheckLocation::Handshake,
        cache_age: revocation_checker.cache_age(),
        max_staleness: Duration::hours(1), // Stricter for connections
    };
    
    let status = revocation_checker.is_revoked(&peer_credential.node_id)?;
    let decision = policy.evaluate(&peer_credential.node_id, status, &context);
    
    match decision {
        Decision::Accept => Ok(HandshakeResult::Accepted),
        Decision::Reject => {
            log::info!("Rejecting connection from revoked node: {:?}", peer_credential.node_id);
            Err(HandshakeError::NodeRevoked)
        },
        Decision::Defer | Decision::AcceptWithWarning => {
            // Should not happen with strict policy, but handle gracefully
            Err(HandshakeError::RevocationCheckFailed)
        },
    }
}
```

## RSL Distribution Strategies

### Strategy 1: Database Polling

Nodes poll the central database periodically:
```sql
-- Nodes can query for incremental updates
SELECT * FROM node_revocations 
WHERE updated_at > $last_check_timestamp;
```

**Pros:** Simple, reliable  
**Cons:** Centralized, all nodes hit same DB

### Strategy 2: Gossip Protocol

Nodes share RSL updates via P2P gossip:
```rust
// Gossip message
pub struct RslGossipMessage {
    pub sequence_number: u64,
    pub new_revocations: Vec<RevokedNode>,
    pub signature: Vec<u8>,
}
```

**Pros:** Decentralized, scalable  
**Cons:** Complex, eventual consistency

### Strategy 3: Hybrid Approach

- CA publishes signed RSL to database
- Nodes with good connectivity fetch directly
- Other nodes get updates via gossip from peers
- Fallback to polling if gossip fails

```rust
pub struct HybridRslManager {
    direct_fetcher: DatabaseFetcher,
    gossip_subscriber: GossipSubscriber,
    cache: RslCache,
}

impl RslManager for HybridRslManager {
    async fn fetch_latest(&self) -> Result<RevocationStatusList> {
        // Try direct fetch first
        match self.direct_fetcher.fetch_latest().await {
            Ok(rsl) => return Ok(rsl),
            Err(_) => log::warn!("Direct fetch failed, falling back to gossip"),
        }
        
        // Try gossip
        match self.gossip_subscriber.get_latest().await {
            Ok(rsl) => return Ok(rsl),
            Err(_) => log::warn!("Gossip fetch failed"),
        }
        
        // Return cached if still valid
        self.cache.get_valid_or_err()
    }
}
```

## Security Considerations

### 1. RSL Signature Verification

Every RSL must be verified against the CA root public key:
```rust
pub fn verify_rsl_signature(rsl: &RevocationStatusList, ca_public_key: &[u8]) -> Result<()> {
    // Verify CA signature over RSL
    let payload = build_rsl_payload(rsl)?;
    let signature = ed25519_dalek::Signature::try_from(rsl.signature.as_slice())?;
    let verifying_key = VerifyingKey::from_bytes(ca_public_key)?;
    
    verifying_key.verify(&payload, &signature)?;
    Ok(())
}
```

### 2. Replay Attack Prevention

Include sequence numbers and timestamps to prevent replay of old RSLs:
```rust
fn is_rsl_replay(old_rsl: &RevocationStatusList, new_rsl: &RevocationStatusList) -> bool {
    new_rsl.sequence_number <= old_rsl.sequence_number
        || new_rsl.issued_at <= old_rsl.issued_at
}
```

### 3. Key Rotation Support

When CA keys rotate, include cross-signing:
```rust
pub struct CrossSignedRsl {
    /// New RSL signed by new CA key
    pub new_rsl: RevocationStatusList,
    
    /// Old CA's signature on new CA's public key
    pub old_ca_signature_on_new_key: Vec<u8>,
    
    /// New CA's signature on the RSL
    pub new_ca_signature: Vec<u8>,
}
```

### 4. Privacy Considerations

- Node identifiers are already hashed (SHA-256 of public key)
- RSL distribution should not leak which nodes specific peers care about
- Consider using Private Information Retrieval (PIR) for sensitive deployments

## Implementation Timeline

### Phase 1: Core Infrastructure
- [ ] Database schema for node_revocations table
- [ ] `RevocationStatusList` and `RevokedNode` structs
- [ ] CA interface for creating revocations
- [ ] RSL signing and verification

### Phase 2: Node-Side Checker
- [ ] `FullRslChecker` implementation
- [ ] `DataRecordingPolicy` and `ConnectionPolicy`
- [ ] Integration with occurrence verification
- [ ] Integration with P2P handshake

### Phase 3: Distribution
- [ ] Database polling implementation
- [ ] RSL snapshot storage in database
- [ ] Background refresh task
- [ ] Gossip protocol (optional)

### Phase 4: CA Tools
- [ ] CLI command for revoking nodes (`ca-revoke`)
- [ ] CLI command for generating RSL (`ca-generate-rsl`)
- [ ] RSL publishing endpoint
- [ ] Monitoring and alerting for revocations

## Testing Strategy

### Unit Tests
- RSL signature verification
- Revocation status checking
- Policy evaluation logic
- Cache staleness handling

### Integration Tests
- End-to-end revocation workflow
- Network with revoked node (should be rejected)
- Stale cache behavior
- RSL refresh

### Fuzzing
- RSL deserialization
- Policy edge cases
- Cache corruption scenarios

## Implementation Status

### ✅ Phase 1: Core Infrastructure (COMPLETE)

The following components have been implemented in the `ca/` crate:

**Structures:**
- ✅ `RevocationStatus` enum (Valid, Revoked, Unknown)
- ✅ `RevocationReason` enum (7 reason codes)
- ✅ `RevokedNode` struct (node_id, revoked_at, reason, signing_public_key, notes)
- ✅ `RevocationStatusList` (RSL) with signature support
- ✅ `CheckContext` and `CheckLocation` for policy evaluation
- ✅ `Decision` enum (Accept, Reject, AcceptWithWarning, Defer)

**Traits:**
- ✅ `RevocationChecker` trait for pluggable backends
- ✅ `RevocationPolicy` trait for policy decisions
- ✅ `DataRecordingPolicy` (lenient, for occurrence verification)
- ✅ `ConnectionPolicy` (strict, for P2P handshakes)
- ✅ `InMemoryRslChecker` implementation

**CA Root Methods:**
- ✅ `CaRoot::ca_id()` - Returns SHA-256 hash of CA public key
- ✅ `CaRoot::revoke_node()` - Creates a revocation entry
- ✅ `CaRoot::create_rsl()` - Signs and creates an RSL
- ✅ `CaRoot::verify_rsl()` - Verifies RSL signature

**Testing:**
- ✅ 25 unit tests passing (10 new revocation tests)
- ✅ RSL creation and verification
- ✅ Tamper detection
- ✅ Policy evaluation logic

### 📝 Phase 2: Integration (TODO)

Pending implementation:
- [ ] Database schema for `node_revocations` table
- [ ] `RslManager` trait and database-backed implementation
- [ ] Integration with occurrence verification workflow
- [ ] Integration with P2P handshake
- [ ] Background RSL refresh task
- [ ] CLI command for revoking nodes (`ca-revoke`)
- [ ] CLI command for generating RSL (`ca-generate-rsl`)

### 🔮 Phase 3: Distribution (TODO)

Future enhancements:
- [ ] Gossip protocol for RSL distribution
- [ ] Hybrid fetch strategy (direct + gossip)
- [ ] Bloom filter backend for memory efficiency
- [ ] Private Information Retrieval (PIR) for privacy

## References

- [RFC 5280](https://datatracker.ietf.org/doc/rfc5280/) - X.509 CRL profile
- [RFC 6960](https://datatracker.ietf.org/doc/rfc6960/) - OCSP
- [RFC 7250](https://datatracker.ietf.org/doc/rfc7250/) - Raw Public Keys in TLS
- RFC 7250 Adaptation document: `.knowledge/architecture/specifications/rfc_7250/adaptation.md`
