## Part 7: Multi-Threading (Web Workers + SharedArrayBuffer)

### 7.1 Worker Architecture

```
Main Thread (UI):
  - DOM event handling
  - UI rendering
  - Main arena (UI nodes)

Worker Thread(s):
  - CPU-heavy computation
  - Separate arena per worker
  - No DOM access
```

### 7.2 SharedArrayBuffer for Zero-Copy Communication

Following MoonZoon's approach, use SharedArrayBuffer for efficient cross-worker communication:

**Required HTTP Headers (COOP/COEP):**
```rust
// In Moon server (backend)
response.headers.insert("Cross-Origin-Opener-Policy", "same-origin");
response.headers.insert("Cross-Origin-Embedder-Policy", "require-corp");
```

**Shared Memory Layout (see K22 for full details):**
```rust
// IMPORTANT: Node state is NOT shared - only message queues!
pub struct SharedRegion {
    pub message_queues: [[AtomicU64; 1024]; MAX_WORKERS],  // SPSC queues
    pub dirty_bitmap: [AtomicU64; 1024],  // Cross-worker dirty notifications
    // NO nodes_region - nodes live in thread-local arenas
}
```

**Benefits:**
- Lock-free message passing between workers (K28 requires NO locks on frontend)
- No serialization overhead for primitives via SharedArrayBuffer
- Node state stays thread-local (simpler, no complex atomic protocols)

### 7.3 Cross-Worker Message Protocol

For complex payloads (Text, Objects), fall back to postMessage:

```rust
#[derive(Serialize, Deserialize)]
pub struct WorkerMessage {
    pub source_worker: WorkerId,
    pub source_slot: SlotId,
    pub target_worker: WorkerId,
    pub target_slot: SlotId,
    pub payload: Payload,
}
```

**Routing:** Main thread routes cross-worker messages via `postMessage` for complex data, SharedArrayBuffer for primitives.

---

