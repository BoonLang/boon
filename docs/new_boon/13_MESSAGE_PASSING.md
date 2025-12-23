## Part 3: Message-Passing Dataflow

### 3.1 Message Format

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub source: NodeAddress,
    pub payload: Payload,
    pub version: u64,
    pub idempotency_key: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Payload {
    // Scalars
    Number(f64),
    Text(Arc<str>),   // Interned for efficiency (Arc<str> is fine - see K2)
    Tag(u32),         // Tag ID from intern table
    Bool(bool),

    // Containers (handles, not storage - see F3)
    ListHandle(SlotId),      // Reference to Bus node
    ObjectHandle(SlotId),    // Reference to Router node

    // Error wrapper (see Part 6 for FLUSH semantics)
    Flushed(Box<Payload>),

    // Deltas for efficient sync (see Section 3.4)
    ListDelta(ListDelta),
    ObjectDelta(ObjectDelta),
}
```

### 3.2 Node Kinds (Hardware Primitives)

| Boon Construct | Node Kind | Hardware Equivalent |
|----------------|-----------|---------------------|
| Constant | `Producer` | Tied signal |
| Variable | `Wire` | Named wire |
| Object | `Router` | Demultiplexer |
| List | `Bus` | Address decoder |
| LATEST | `Combiner` | Multiplexer |
| HOLD | `Register` | D flip-flop |
| THEN | `Transformer` | Combinational logic |
| WHEN | `PatternMux` | Pattern decoder |
| WHILE | `SwitchedWire` | Tri-state buffer |
| LINK | `IOPad` | I/O port |
| TEXT { {expr} } | `TextTemplate` | String formatter with deps (see K4) |
| Cross-domain | `TransportEdge` | I/O buffer with handshake |

### 3.2.1 TransportEdge (Cross-Domain Routing)

Cross-domain edges become explicit `TransportEdge` nodes:

```rust
#[derive(Clone, Debug)]
pub struct TransportEdge {
    pub source_domain: Domain,
    pub target_domain: Domain,
    pub source_node: NodeAddress,
    pub target_node: NodeAddress,

    // Protocol state
    pub next_seq: u64,
    pub pending_acks: VecDeque<(u64, Message)>,  // (seq, msg) for retry
    pub last_ack_seq: u64,
}

impl TransportEdge {
    fn send(&mut self, msg: Message) -> TransportMessage {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending_acks.push_back((seq, msg.clone()));

        TransportMessage {
            seq,
            idempotency_key: msg.idempotency_key,
            payload: msg.payload,
        }
    }

    fn on_ack(&mut self, ack_seq: u64) {
        self.pending_acks.retain(|(seq, _)| *seq > ack_seq);
        self.last_ack_seq = ack_seq;
    }

    fn resync_from(&self, from_seq: u64) -> Vec<TransportMessage> {
        // Replay messages since from_seq for reconnection
        self.pending_acks
            .iter()
            .filter(|(seq, _)| *seq >= from_seq)
            .map(|(seq, msg)| TransportMessage { seq: *seq, .. })
            .collect()
    }
}
```

**Benefits:**
- Explicit cross-domain edges (not implicit network magic)
- Transport logic is isolatable and testable
- Natural place for retry, buffering, reconnection
- Same pattern works for WebWorker and WebSocket

### 3.3 HOLD State Restrictions

**HOLD (Register) stores only scalar/simple values:**

| Allowed in HOLD | NOT Allowed |
|-----------------|-------------|
| Number | List / Bus |
| Text | Nested Objects |
| Tag | Objects containing List |
| Flat Object (fields are Number/Text/Tag only) | Recursive structures |

**Why:** Register is a D flip-flop - one storage cell for one value. Collections use Bus instead.

**Pattern:**
```boon
-- CORRECT: scalar state in HOLD
counter: 0 |> HOLD state { trigger |> THEN { state + 1 } }
editing: None |> HOLD state { start |> THEN { Editing[id] } }

-- CORRECT: collection in LIST (not HOLD)
items: LIST {} |> List/append(item: new_item)

-- WRONG: List in HOLD
items: LIST {} |> HOLD state { ... }  -- NOT ALLOWED
```

### 3.4 Delta Streams for Containers (Scalability)

**Problem:** Materialized values don't scale for large lists with nested data.

```
500 users, one status change:
  Materialized: Diff O(500), copy nested data, send 5KB over WebSocket
  Delta stream: Emit delta, O(1), send 50 bytes
```

**Solution:** Containers emit delta events, not full values.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ListDelta {
    Insert { key: ItemKey, index: u32, initial: Payload },
    Update { key: ItemKey, field: FieldId, value: Payload },
    Remove { key: ItemKey },
    Move { key: ItemKey, from_index: u32, to_index: u32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ObjectDelta {
    FieldUpdate { field: FieldId, value: Payload },
}
```

**How it works:**

```
User status changes:
  1. HOLD emits new status value
  2. Parent Router (user object) wraps in ObjectDelta::FieldUpdate
  3. Parent Bus (users list) wraps in ListDelta::Update { key: user_123, field: "status", ... }
  4. Subscribers receive delta, update only affected part
  5. WebSocket: serialize just the delta
```

**Materialization points:**
- UI rendering (convert deltas to DOM ops)
- Snapshots (accumulate deltas into full state)
- WHEN pattern matching (may need full value)

**Benefits for backend:**
- WebSocket sync: O(delta_size) not O(list_size)
- Reconnect: send snapshot + replay deltas from checkpoint
- Nested lists: independent delta streams, no parent copying

### 3.5 Routing Table

Explicit routing instead of implicit subscriptions:

```rust
pub struct RoutingTable {
    // Static routes (from source code)
    static_routes: HashMap<NodeAddress, Vec<NodeAddress>>,
    // Dynamic routes (from WHILE, List changes)
    dynamic_routes: HashMap<NodeAddress, Vec<NodeAddress>>,
}
```

**Files to modify:**
- `crates/boon/src/platform/browser/engine.rs` - Message, Payload, RoutingTable types

---

