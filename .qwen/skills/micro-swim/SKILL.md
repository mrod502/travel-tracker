---
name: micro-swim
description: Lightweight gossip protocol (μswim) for resource-constrained edge devices
source: learned
---

# μswim: Lightweight Gossip Protocol for Edge Devices

A SWIM-inspired gossip protocol designed for extreme-edge (EE) devices with limited RAM (C2-C3 class: 50KB-1MB), enabling decentralized peer-to-peer communication without centralized brokers.

**Repository:** https://github.com/daubaris/microswim

## When to Use

- **Extreme-edge device coordination**: When deploying gossip/membership protocols on constrained devices (ESP32, STM32) with 50KB-1MB RAM
- **Decentralized discovery**: When you need peer-to-peer node discovery without brokers or servers
- **Federated/gossip learning**: When enabling edge devices to exchange model updates in a decentralized manner
- **Collective sensing**: When aggregating sensor data from multiple EE devices without central coordination
- **Cost-sensitive IoT deployments**: When reducing packet load by 45-50% translates to operational savings at scale

**Do NOT use when:**
- You have resource-rich servers only (use Consul/Serf instead)
- Deterministic message delivery is required (gossip is probabilistic)
- You need immediate consistency (eventual consistency only)

## Core Protocol Design

### Two Mandatory Tasks

1. **Failure Detection Task** (Algorithm 1):
   - Runs once per protocol period
   - Selects k nodes round-robin from known members
   - Constructs gossip message with piggybacked member updates
   - Encodes and sends to selected peers
   - Records sent pings for tracking

2. **Listener Task** (Algorithm 2):
   - Runs continuously (more frequent than failure detection)
   - Checks for expired PING messages
   - Marks unresponsive nodes as SUSPECT
   - Requests indirect pings from other nodes
   - Receives and decodes incoming messages
   - Handles PING→ACK, EVENT propagation

### Message Structure

```
Mandatory fields:
├── Message type (PING, ACK, EVENT, etc.)
├── Update count (number of member updates)
├── Origin node info
└── Updates array (piggybacked member states)

Optional fields:
├── Event type (for custom events)
└── User-defined payload
```

### Node States

- **ALIVE**: Responding normally
- **SUSPECT**: Missed acknowledgment, temporary timeout (20s default)
- **CONFIRMED**: Confirmed dead after suspect timeout

**Incarnation numbers** prevent state inconsistency - incremented when node learns it was suspected.

## Procedure

### 1. Setup Message Encoding

Choose between CBOR or JSON based on trade-offs:

```
CBOR (libcbor):
✓ ~22% smaller messages
✓ Better for large piggyback arrays
✗ ~2x encoding latency
✗ Slower decoding

JSON (jsmn/snprintf):
✓ Faster encoding/decoding
✓ Human-readable
✗ Larger message size
✗ Bandwidth inefficient
```

**Recommendation:** Use CBOR for production (total pipeline time is better despite slower encoding).

### 2. Configure Protocol Parameters

Key tunable parameters:

```
GOSSIP_FANOUT [2-4]: Number of peers to ping per period
  - Higher = faster convergence, more messages
  - Baseline: fanout=2

NODES_PER_MESSAGE [2-4]: How many members to share per message
  - Higher = faster convergence
  - Trade-off: larger messages

PROTOCOL_PERIOD [1s]: Time between gossip rounds
  - Shorter = faster but more energy
```

**Convergence benchmarks** (128 nodes, CBOR encoding):
- Fanout 2, 2 nodes/msg: ~64 rounds
- Fanout 4, 4 nodes/msg: ~24 rounds (63% faster)

### 3. Implement Platform-Specific Tasks

The protocol is written in C for portability (POSIX and RIOT OS compatible):

```c
// Example: Failure detection loop
while (true) {
    // Round-robin select peer
    member = RetrieveMember(member_state);
    
    // Select k updates to piggyback
    count = RetrieveUpdates(member_state, updates);
    
    // Construct and encode message
    ConstructMessage(&msg, updates, count);
    len = EncodeMessage(&msg, buffer);  // CBOR or JSON
    
    // Send via UDP
    SendMessage(buffer, len);
    AddPing(member);  // Track for timeout
    
    Sleep(PROTOCOL_PERIOD);
}
```

### 4. Add Custom Events (Optional)

For application-specific triggers:

```c
// Define event types
typedef enum {
    EVENT_DATA_LOW_ACCURACY = 0,  // Request inference help
    EVENT_MODEL_UPDATE,           // Share model update
    EVENT_SENSOR_READING          // Broadcast sensor data
} EventType;

// Trigger event when threshold met
if (accuracy_score < THRESHOLD) {
    Event e = {
        .type = EVENT_DATA_LOW_ACCURACY,
        .payload = &current_measurement,
        .payload_size = sizeof(current_measurement)
    };
    SendCustomEvent(&e);
}
```

### 5. Handle Membership Changes

Monitor node states for your application logic:

```c
void HandleMemberUpdate(MemberInfo *m) {
    switch (m->state) {
        case ALIVE:
            // Node available for tasks
            RegisterAvailableNode(m);
            break;
        case SUSPECT:
            // Still use but mark as unreliable
            MarkUnreliable(m);
            break;
        case CONFIRMED:
            // Node dead, clean up
            RemoveNode(m->id);
            break;
    }
}
```

## Performance Characteristics

### Energy Consumption
- Matches CoAP and MQTT (~310J over 30 min on ESP32)
- **45-50% fewer packets** than centralized protocols
- Similar cumulative energy despite decentralization

### Convergence Time
- 8 nodes: ~3-5 rounds (~3-5 seconds)
- 128 nodes: ~24-64 rounds (~24-64 seconds)
- Improves with higher fanout and nodes_per_message

### Membership Accuracy
- **0% packet loss**: Negligible false positives
- **10% packet loss**: 
  - Suspect FP rate increases 4-11x
  - Confirmed FP only at 64+ nodes (~1.3 per node)

### Memory Footprint (RIOT OS, STM32F7)
- Total: ~55KB (.text: 30KB, .data: 8KB, .bss: 24KB)
- Main overhead: lwIP networking stack
- Fits on C2-C3 class devices (50KB+ code space)

## Pitfalls

### 1. High False Positives Under Packet Loss
**Problem:** At 10% packet loss, confirmed false positives appear in larger networks, permanently removing alive nodes.

**Mitigation:** 
- Implement Lifeguard-style adjustments for slow processing
- Increase suspicion timeout for unstable networks
- Use 2-phase confirmation (suspect→probe→confirmed)

### 2. Message Size vs. Network MTU
**Problem:** Large piggyback arrays can exceed UDP payload limits on constrained networks.

**Mitigation:**
- Limit NODES_PER_MESSAGE to 2-4 on low-bandwidth networks
- Monitor MTU of underlying transport (e.g., 1280 bytes for IPv6 over 6LoWPAN)

### 3. Encoding Performance Gap
**Problem:** libcbor has ~2x encoding latency vs. snprintf for JSON.

**Mitigation:**
- Accept the trade-off: CBOR's smaller size wins for total pipeline time
- Pre-allocate message buffers to avoid dynamic allocation during gossip period

### 4. Converging Large Networks
**Problem:** 128+ physical devices impractical to test; Docker bridge network has known issues.

**Mitigation:**
- Use Docker host network for convergence testing
- Simulate packet loss via tc (traffic control) instead of actual drops

### 5. State Inconsistency During Rejoin
**Problem:** Nodes that temporarily disconnect may have stale incarnation numbers.

**Mitigation:**
- Increment incarnation on every state change
- Always include full member state in EVENT messages, not just deltas

## Tuning Guide

| Scenario | Fanout | Nodes/Msg | Period | Rationale |
|----------|--------|-----------|--------|-----------|
| Low bandwidth | 2 | 2 | 2s | Minimize traffic |
| Fast discovery | 4 | 4 | 1s | Maximize speed |
| Balanced (default) | 3 | 3 | 1s | Good trade-off |
| High churn | 4 | 2 | 0.5s | Quick detection |
| Stable network | 2 | 4 | 2s | Efficient updates |

## Cost Savings at Scale

For pay-per-MB IoT plans ($0.01-$0.10/MB):

| Fleet Size | Monthly Savings vs. CoAP | Annual Savings |
|------------|--------------------------|----------------|
| 100 devices | 155 MB | $186-$1,860 |
| 500 devices | 775 MB | $930-$9,300 |
| 1000 devices | 1.55 GB | $1,860-$18,600 |

## Use Cases

1. **Federated/Gossip Learning**: Exchange model updates without central server
2. **Collective Sensing**: Aggregate environmental data across device fleet
3. **Inference Offloading**: Discover nearby nodes with better compute
4. **Service Discovery**: Find devices offering specific capabilities
5. **Health Monitoring**: Track availability of edge infrastructure

## References

- **Paper**: Computer Networks 287 (2026) 112539
- **Original SWIM**: Das et al., DSN 2002
- **IETF Constrained Nodes**: RFC 9106 (Terminology for Constrained-Node Networks)
- **RIOT OS**: https://riot-os.org/
