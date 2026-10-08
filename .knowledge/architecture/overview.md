# Architecture Overview

## System Goal

Distributed BLE device tracking network that:
1. Scans Bluetooth advertisement traffic across multiple geographic locations
2. Associates observations with GPS location
3. Detects co-location patterns between devices
4. **Tracks devices only — no person identity linkage anywhere in the system**

## Node Tiers

### Full Node
- **Storage:** Local Postgres/PostGIS, geo-partitioned
- **Compute:** Full API, aggregation jobs, association detection
- **Network:** MQTT (+ optional mesh gateway)
- **Role:** Owns a partition of data, answers federated queries
- **Build Priority:** Phase 0 (build first)

### Light Node *(Future - Phase 2)*
- **Storage:** None or cache only
- **Compute:** Scan + forward
- **Network:** MQTT
- **Role:** Scans, forwards raw occurrences to nearest full node

### Signal Node
- **Storage:** None
- **Compute:** Minimal — capture + sign ping
- **Network:** LoRa/Meshtastic
- **Role:** Cheap coverage extension. **Stationary, pre-registered with CA.**
- **Build Priority:** Phase 4

### Aggregator Node
- **Storage:** None or cache
- **Compute:** Mesh→MQTT bridge, enrichment
- **Network:** LoRa/Meshtastic + MQTT
- **Role:** Translates signal-node pings into full occurrence records
- **Build Priority:** Phase 4

## Coordination Model

### Decentralized Gossip
- No central always-on registry for node liveness
- Full nodes use **gossip-based membership** (SWIM protocol) for failure detection and peer discovery
- Signal/aggregator nodes don't participate in gossip — they report through an aggregator or full node

### Scaling Concerns
- Full mesh gossip subscription doesn't scale indefinitely
- Need to revisit hub/relay pattern once node count grows past a few dozen
- **Open question:** What is the scaling threshold?

## Identity & Trust

### mTLS Transport Layer
- CA (e.g., **step-ca**) issues **short-lived certs** to full/aggregator/light nodes
- Auto-renewed via mTLS
- Short expiry means compromised node's transport access lapses naturally
- Avoids needing CRL/OCSP distribution across decentralized network
- CA required only at **enrollment**, not for ongoing operation

### Node Identity
- `node_id` is **self-certifying** — SHA-256 digest of node's own signing public key
- Not independently allocated value
- Separate cosmetic `display_name` for logs/UI
- Compact `short_id` exists purely for LoRa wire-budget (signal nodes only)

### Signing Keys
- **Every node tier** (including signal nodes) issued Ed25519 signing keypair at enrollment
- Separate from mTLS cert (signaling nodes never do TLS handshake over LoRa)
- Used to sign individual occurrence observations for provenance
- **Open question:** No rotation cadence defined yet (mTLS rotates short-lived, signing keys don't)

## Multi-Aggregator Overlap & Conflict Resolution

When multiple aggregators hear the same signal-node ping (desired for coverage redundancy):

1. **Deterministic occurrence_id:** `hash(signal_node_id + sequence_num + device_hash + timestamp_offset)`
   - Not randomly generated per-aggregator
   - Every aggregator computes the **same ID** for the same ping

2. **Deduplication:** `INSERT ... ON CONFLICT DO NOTHING` at full-node DB layer
   - No coordination needed between aggregators

3. **Identical signatures:** Both aggregators' rows carry identical `signed_payload`/`signature`
   - Enables both dedup AND independent provenance verification

## Data Flow Summary

1. **BLE device** emits advertisement
2. **Signal node** captures, rate-limits, builds compact `SignalPing`, signs, broadcasts over LoRa
3. **Aggregator(s)** hear ping, verify signature, enrich with fixed location/timestamp, publish to MQTT
4. **MQTT broker** routes by geographic topic to responsible full node(s)
5. **Full node** writes occurrence (`INSERT ... ON CONFLICT DO NOTHING`)
6. **Full nodes sync** with peers via application-level batched sync over MQTT

## Transport Choices

### MQTT (Full/Aggregator ↔ Full Nodes)
- Topic scheme: `occurrences/{geo_cell_macro}/{node_id}`
- QoS 1 (at-least-once) — sufficient because dedup handled by `ON CONFLICT`
- Hierarchical by geography so full nodes only subscribe to relevant traffic

### LoRa/Meshtastic (Signal ↔ Aggregator)
- Payload budget: ~200-256 bytes
- `SignalPing` uses compact binary/protobuf format (not JSON)
- **Open question:** Truncated `device_hash_short` byte length not finalized (recommendation: 10-12 bytes for NYCMesh-scale density)

## Consistency Model

**Eventual consistency** is acceptable:
- Append-mostly sensor data
- Conflicts avoided rather than resolved
- Each occurrence written exactly once, never mutated
- Store-and-forward for intermittently-connected nodes

## Retention Strategy

1. **Write-time rate limiting:** 10-20s max report frequency per device, per node
   - Enforced at edge before write
   - Not a DB constraint

2. **Two-tier retention:**
   - Raw `occurrences` for N days (N TBD from Phase 0 volume testing)
   - Roll up into `occurrence_rollups` summary rows
   - Drop raw partition

## Partitioning Strategy

### Physical (within each full node's Postgres)
1. **RANGE on `observed_at`** (monthly) — retention/compaction
2. **LIST on `geo_cell_macro`** (H3 res 6) — storage locality
   - Node holds sub-partitions only for cells it owns
   - Queries prune on geography natively

### Logical (across network)
- `owns_geo_cells` determines which macro-cell sub-partitions each node provisions
- Application-level sharding boundary layered on physical partitioning

**Operational consequence:** Adding a new owned macro cell requires provisioning a new `LIST` sub-partition (automatable, but not automatic). Rows for unprovisioned cells fall into `DEFAULT` catch-all partition.

## Phase Roadmap

| Phase | Goal | Exit Criteria |
|-------|------|---------------|
| **Phase 0** | Single-node prototype | Query "what did this node see in the last hour" and "where" |
| **Phase 1** | Two-node sync | Nodes converge after partition/reconnect cycle |
| **Phase 2** | Federated query | Query across ≥2 nodes returns merged results |
| **Phase 3** | Association detection | Synthetic co-located pairs correctly flagged |
| **Phase 4** | Signal/aggregator tier | Two aggregators hearing same ping → one stored occurrence |
| **Phase 5** | N full nodes at scale | Load-test replication and query fan-out |
| **Phase 6** | Hardening | Legal review, observability, retention policy finalized |

## Security Considerations

- mTLS enforcement across all node-to-node traffic (Phase 6)
- Encryption at rest (Phase 6)
- Access control on APIs (Phase 6)
- **Legal review required** before wider deployment (Phase 6)

## Privacy by Design

- Raw occurrence records contain device signal + location only
- No device-to-person linkage exists in schema at any layer
- Device identity resolution is derived layer (periodic batch job)
- Association detection is derived layer (periodic batch job)
- Both derived layers reprocessable as logic improves

## Known Limitations

### Provenance Signature Scope
- **Covered:** What origin node observed and asserted
- **NOT covered:** Fields aggregator adds during enrichment (aggregator-assigned location, sync-corrected timestamp)
- **Mitigation:** Trust based on `reporting_node_id` identity, not cryptographic guarantee
- **Open question:** Should aggregator enrichment get its own signature layer?

### Revocation
- `nodes.status = 'revoked'` is local/gossiped flag, not instantaneously consistent
- Verification should check both signature AND status
- Short-lived certs mitigate but don't eliminate delay

---

## References

- **Data Model:** [`data-model.md`](../architecture/data-model.md)
- **Storage:** [`storage.md`](../architecture/storage.md)
- **Provenance:** [`provenance.md`](../provenance.md)
- **Data Flow:** [`data-flow.md`](../data-flow.md)
- **Research Topics:** [`research-topics.md`](../open-questions/research-topics.md)
- **Roadmap:** [`roadmap.md`](../implementation/roadmap.md)
