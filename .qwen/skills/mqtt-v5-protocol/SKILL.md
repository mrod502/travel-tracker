---
name: mqtt-v5-protocol
description: MQTT v5 protocol reference and rust-mqtt library usage
source: learned
---

# MQTT v5 Protocol and rust-mqtt Implementation

## When to Use

- Implementing MQTT client or server functionality
- Working with IoT messaging patterns (publish/subscribe)
- Understanding MQTT packet formats and QoS levels
- Integrating the `rust-mqtt` crate (v0.5.1) into Rust applications
- Implementing connection lifecycle management (CONNECT, DISCONNECT, Will messages)
- Handling subscriptions with topic filters and QoS negotiation

## Protocol Fundamentals

### Core Concepts

**Publish/Subscribe Pattern:**
- Clients connect to a broker (server)
- Publishers send messages to topics
- Subscribers receive messages from topics they subscribe to
- Decoupling: publishers don't know about subscribers and vice versa

**Connection Lifecycle:**
1. **CONNECT** - Establish connection with client identifier, credentials, optional Will
2. **Active Session** - Exchange PUBLISH, SUBSCRIBE, UNSUBSCRIBE packets
3. **DISCONNECT** - Graceful termination or timeout-based disconnection
4. **Will Message** - Automatically sent by broker if client disconnects unexpectedly

**QoS (Quality of Service) Levels:**
- **QoS 0 (At Most Once)**: Fire-and-forget, no acknowledgment
- **QoS 1 (At Least Once)**: Guaranteed delivery, possible duplicates (requires PUBACK)
- **QoS 2 (Exactly Once)**: Guaranteed delivery without duplicates (requires PUBREC, PUBREL, PUBCOMP)

### MQTT v5 Packet Types

| Packet Type | Direction | Description |
|-------------|-----------|-------------|
| CONNECT | Client → Server | Initiate connection, credentials, Will info |
| CONNACK | Server → Client | Connection acknowledgment with return code |
| PUBLISH | Both | Publish message to topic |
| PUBACK | Both | Publish acknowledgment (QoS 1) |
| PUBREC | Both | Publish received (QoS 2, part 1) |
| PUBREL | Both | Publish release (QoS 2, part 2) |
| PUBCOMP | Both | Publish complete (QoS 2, part 3) |
| SUBSCRIBE | Client → Server | Subscribe to topic filters |
| SUBACK | Server → Client | Subscribe acknowledgment |
| UNSUBSCRIBE | Client → Server | Unsubscribe from topics |
| UNSUBACK | Server → Client | Unsubscribe acknowledgment |
| PINGREQ/PINGRESP | Both | Keep-alive heartbeat |
| DISCONNECT | Both | Graceful disconnect notification |

### CONNECT Packet Structure

**Fixed Header:**
- Packet Type: CONNECT
- Remaining Length: Variable

**Variable Header:**
- Protocol Name: "MQTT" (UTF-8 encoded)
- Protocol Version: 5 (byte value)
- Connect Flags:
  - Username Flag
  - Password Flag
  - Will Flag
  - Will QoS (2 bits)
  - Will Retain Flag
  - Clean Start Flag
  - Reserved Flag

**Will Properties (v5 only):**
- Will Delay Interval
- Will Format Indicator (Content-Type)
- Will Correlation Data
- Subscription Identifier
- User Properties

**Payload:**
- Client ID (UTF-8)
- Will Topic + Will Message (if Will Flag set)
- Username (if Username Flag set)
- Password (if Password Flag set)

### PUBLISH Packet Structure

**Fixed Header:**
- Packet Type: PUBLISH
- DUP Flag (duplicate delivery)
- QoS (2 bits)
- RETAIN Flag
- Remaining Length

**Variable Header:**
- Topic Name (UTF-8)
- Packet Identifier (only for QoS 1 and 2)

**PUBLISH Properties (v5):**
- Payload Format Indicator
- Message Expiry Interval
- Content-Type
- Response Topic
- Correlation Data
- Subscription Identifier
- User Properties

**Payload:**
- Application message (binary)

### SUBSCRIBE Packet Structure

**Fixed Header:**
- Packet Type: SUBSCRIBE
- Remaining Length

**Variable Header:**
- Packet Identifier (for matching SUBACK)

**SUBSCRIBE Properties (v5):**
- Subscription Identifier
- No Local Flag
- Retain As Published Flag
- Retain Handling
- User Properties

**Payload:**
- List of (Topic Filter, Subscription Options) pairs
- Subscription Options include QoS and flags

### Key v5 Features

**Reason Codes:**
- Each packet type has associated reason codes
- More granular error reporting than v3.1.1
- Examples: 0x00 (Success), 0x80 (Unspecified error), 0x83 (Not authorized)

**Shared Subscriptions:**
- Format: `$share/{group-name}/{topic-filter}`
- Enables load balancing across subscriber instances

**Message Expiry:**
- Messages can have TTL via Message Expiry Interval property
- Broker discards expired messages

**Topic Aliases:**
- Reduce packet size by using numeric aliases instead of full topic names

**Server Reference:**
- Server can suggest alternative server on CONNACK or DISCONNECT

## Procedure (rust-mqtt Crate Usage)

### 1. Add Dependency

```toml
[dependencies]
rust-mqtt = "0.5.1"
```

### 2. Core Types Overview

**Client Structure:**
```rust
use rust_mqtt::client::struct::Client;
use rust_mqtt::config::struct::{ClientConfig, ServerConfig, SharedConfig};
use rust_mqtt::types::struct::{TopicName, TopicFilter};
use rust_mqtt::types::enum::QoS;
```

**Configuration Types:**
- `ClientConfig` - Client-side configuration
- `ServerConfig` - Server-side configuration
- `SharedConfig` - Shared state configuration
- `ConnectOptions` - Connection parameters
- `WillOptions` - Will message configuration
- `PublicationOptions` - Publish message options
- `SubscriptionOptions` - Subscription parameters

**Event Types:**
- `Publish` - Incoming/outgoing publish event
- `Puback` - QoS 1 acknowledgment
- `Pubrej` - QoS 2 rejection
- `Suback` - Subscription acknowledgment

**Error Handling:**
- `MqttError` - MQTT-specific errors
- `ReasonCode` - v5 reason codes

### 3. Basic Connection Pattern

```rust
use rust_mqtt::client::struct::Client;
use rust_mqtt::config::struct::ClientConfig;
use rust_mqtt::types::struct::TopicName;
use rust_mqtt::types::enum::QoS;

// Create client with configuration
let client = Client::with_config(client_config, buffer_provider);

// Connect to broker
client.connect(connect_options).await?;

// Subscribe to topic
client.subscribe(topic_filter, subscription_options).await?;

// Publish message
client.publish(topic_name, QoS::AtLeastOnce, payload).await?;

// Disconnect gracefully
client.disconnect(disconnect_options).await?;
```

### 4. Event Processing

```rust
use rust_mqtt::client::enum::Event;

// Process incoming events
match event {
    Event::Publish(publish) => {
        let topic = publish.topic();
        let payload = publish.payload();
        // Handle message
    }
    Event::Suback(suback) => {
        // Handle subscription acknowledgment
    }
    Event::Puback(puback) => {
        // Handle QoS 1 acknowledgment
    }
    // ... other events
}
```

### 5. Topic Handling

```rust
use rust_mqtt::types::struct::TopicName;
use rust_mqtt::types::struct::TopicFilter;

// Valid topic name (no wildcards)
let topic = TopicName::new("sensors/temperature/room1")?;

// Topic filter (allows wildcards)
let filter = TopicFilter::new("sensors/temperature/#")?;

// Single-level wildcard: +
// Multi-level wildcard: # (must be at end)
```

### 6. QoS Handling

```rust
use rust_mqtt::types::enum::QoS;
use rust_mqtt::types::enum::IdentifiedQoS;

// QoS levels
let qos0 = QoS::AtMostOnce;      // 0
let qos1 = QoS::AtLeastOnce;     // 1
let qos2 = QoS::ExactlyOnce;     // 2

// QoS with packet identifier (for acknowledged delivery)
let identified = IdentifiedQoS::new(qos1, packet_id);
```

### 7. Configuration Options

```rust
use rust_mqtt::client::options::ConnectOptions;
use rust_mqtt::client::options::WillOptions;
use rust_mqtt::types::enum::KeepAlive;

// Connection options
let connect_opts = ConnectOptions {
    client_identifier: "my-client".to_string(),
    clean_start: true,
    keep_alive: KeepAlive::from_seconds(60),
    will: Some(WillOptions {
        topic: "clients/my-client/status".into(),
        message: "offline".as_bytes().into(),
        qos: QoS::AtLeastOnce,
        retain: true,
        ..Default::default()
    }),
    // ... other options
};
```

## Pitfalls

### Common Failure Modes

1. **QoS Level Mismatch**
   - Server may downgrade QoS if it doesn't support requested level
   - Check Maximum QoS property in CONNACK

2. **Topic Wildcard Misuse**
   - `#` must be the last character and preceded by `/`
   - `+` replaces exactly one level
   - Topic names cannot contain wildcards; filters can

3. **Keep Alive Timeout**
   - Client must send PINGREQ if no message within 50% of keep alive interval
   - Server disconnects if no traffic within 1.5x keep alive

4. **Packet Identifier Exhaustion**
   - QoS 1 and 2 require unique packet identifiers
   - Must wait for acknowledgment before reusing identifier
   - rust-mqtt: `MqttError::PacketIdentifierAwaitingPubcomp`

5. **Clean Start vs Session State**
   - `CleanStart=true`: Start fresh, no session state
   - `CleanStart=false`: Resume existing session (if server supports)
   - v5 adds explicit Session Expiry Interval

6. **Will Message Security**
   - Will can contain sensitive data
   - Consider encryption for Will messages
   - Will can be exploited for DoS if not rate-limited

7. **Buffer Management**
   - rust-mqtt requires a `BufferProvider` trait implementation
   - In-flight messages consume buffer space
   - Configure `ReceiveMaximum` and `SendQuota` appropriately

8. **v5 Property Limits**
   - Maximum Packet Size must be respected
   - Topic Alias maximum is negotiated
   - User Properties have size limits

### Error Recovery

```rust
use rust_mqtt::types::enum::MqttError;

match mqtt_operation.await {
    Ok(result) => result,
    Err(MqttError::Network) => {
        // Reconnect logic
        client.reconnect().await?;
    }
    Err(MqttError::RecoveryRequired) => {
        // Session state corrupted, need full reset
    }
    Err(MqttError::ServerMaximumPacketSizeExceeded) => {
        // Reduce payload size or use topic aliases
    }
    Err(e) => return Err(e.into()),
}
```

## References

- **MQTT v5.0 OASIS Standard**: `.knowledge/architecture/mqtt-v5.md`
- **rust-mqtt Documentation**: `target/doc/rust_mqtt/`
- **rust-mqtt Crate**: Version 0.5.1
