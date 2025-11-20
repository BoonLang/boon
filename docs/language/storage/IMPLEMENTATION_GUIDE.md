# Boon Storage Implementation Guide

Practical guidance for implementing Boon's storage layer: decision matrices, open questions, potential pitfalls, and domain-specific recommendations.

---

## Decision Matrices

### When to Use Which Technology?

#### Incremental Computation Engine

| Use Case | Choose | Why |
|----------|--------|-----|
| **Server with SQL/Datalog queries** | DBSP (Feldera) | ✅ Compiles SQL/Datalog directly<br>✅ Query optimizer<br>✅ Formally verified<br>✅ Enterprise-grade<br>⚠️ Distributed runtime still in development |
| **Browser/WASM incremental queries** | Differential Dataflow | ✅ Pure Rust (WASM-compatible)<br>✅ Proven at scale (Materialize)<br>✅ MIT license<br>⚠️ Rust API only (no SQL) |
| **Small datasets (hardware/compiler)** | Datafrog | ✅ Extremely fast for small data<br>✅ Simple API<br>✅ Used in Rust compiler<br>❌ No distributed support<br>❌ No incremental computation |
| **Research/experimentation** | Differential Datalog (DDlog) | ✅ High-level Datalog language<br>✅ Compiles to Differential Dataflow<br>⚠️ Archived (VMware discontinued)<br>⚠️ Less maintained |

**Recommendation for Boon:**
- **Server:** DBSP/Feldera (when distributed ready) or RisingWave
- **Software:** Differential Dataflow
- **Hardware:** Datafrog or custom (CAM/BRAM)

---

#### Streaming Database

| Use Case | Choose | Why |
|----------|--------|-----|
| **Production server with S3 storage** | RisingWave | ✅ S3 as primary storage<br>✅ Distributed from day 1<br>✅ Postgres-compatible<br>✅ Apache 2.0 license<br>✅ Production-ready HA |
| **Need Differential Dataflow under hood** | Materialize | ✅ Built on Differential Dataflow<br>✅ Frank McSherry's team<br>⚠️ BSL license (restrictions!)<br>⚠️ Clustered deployment limits |
| **Flink replacement (stream processing)** | Arroyo | ✅ 10x faster than Flink (sliding windows)<br>✅ SQL + Rust pipelines<br>✅ Kubernetes-native<br>✅ Apache 2.0 license |
| **Research (formal semantics)** | Hydroflow | ✅ Formal verification (POPL 2025)<br>✅ UC Berkeley research<br>⚠️ Research project (not production) |

**Recommendation for Boon:**
- **Production server:** RisingWave (best S3 integration, fully open source)
- **Inspiration/reference:** Materialize (but don't use due to BSL)
- **Stream processing:** Arroyo (if needed for Kafka/streaming)

---

#### Storage Backend

| Use Case | Choose | Why |
|----------|--------|-----|
| **Large blobs (BYTES, files, video)** | S3 + RisingWave | ✅ S3 as primary storage<br>✅ Optimized multipart upload<br>✅ Hybrid caching (Foyer)<br>✅ Cost-effective |
| **Triple store metadata** | DBSP/Feldera or Differential Dataflow | ✅ Incremental queries<br>✅ Fast updates<br>✅ In-memory or spillable |
| **Server persistence** | Postgres (via RisingWave/Feldera) | ✅ EAV table for triples<br>✅ Logical replication (ElectricSQL)<br>✅ Battle-tested |
| **Browser persistence** | IndexedDB + NATS sync | ✅ Local storage<br>✅ NATS for distributed sync<br>✅ Works offline |
| **Hardware** | BRAM/CAM | ✅ Parallel lookup (CAM)<br>✅ Small, fixed-size<br>✅ Low latency |

**Recommendation for Boon:**
```
Metadata (triples):     Differential Dataflow (software) / DBSP (server)
Large blobs (BYTES):    NATS Object Store (distributed) / S3 (server)
Persistence:            IndexedDB (browser) / Postgres+S3 (server)
Hardware:               BRAM (storage) / CAM (lookup)
```

---

### Decision Tree: Choose Your Stack

```
START: What domain are you targeting?
  │
  ├─ HARDWARE
  │    │
  │    ├─ Small dataset (< 1000 entries)?
  │    │    └─→ Datafrog + BRAM
  │    │
  │    └─ Larger dataset?
  │         └─→ CAM (parallel lookup) or Hash+BRAM
  │
  ├─ SOFTWARE (Browser/Desktop)
  │    │
  │    ├─ Need incremental queries?
  │    │    └─→ Differential Dataflow + IndexedDB
  │    │
  │    ├─ Need distributed sync?
  │    │    └─→ + NATS (KV + Object Store)
  │    │
  │    └─ Large blobs (BYTES)?
  │         └─→ NATS Object Store (chunks)
  │
  └─ SERVER
       │
       ├─ Need SQL/Datalog?
       │    └─→ DBSP/Feldera (when distributed) or RisingWave
       │
       ├─ Need S3 storage?
       │    └─→ RisingWave (S3 as primary)
       │
       ├─ Need stream processing?
       │    └─→ Arroyo (Kafka, etc.)
       │
       └─ Need distributed cluster?
            └─→ RisingWave or Arroyo (Kubernetes)
```

---

## Open Questions

### Technical Unknowns

**1. WASM Compilation**

**Question:** Can DBSP/Feldera compile to WASM for browser use?

**Why it matters:** If yes, we could use DBSP for both server and browser (unified). If no, we need Differential Dataflow for browser.

**Investigation needed:**
- Check Feldera WASM support
- Try compiling DBSP to WASM
- Benchmark performance vs native

**Fallback:** Use Differential Dataflow for browser (known WASM-compatible).

---

**2. Triple Store Performance**

**Question:** EAV (Entity-Attribute-Value) vs Columnar storage - which is faster for incremental queries?

**Trade-offs:**
```
EAV Table:
  ✅ Schema-less (easy to add attributes)
  ✅ Perfect for triple store model
  ⚠️ Potentially slower joins (more rows)

Columnar:
  ✅ Fast aggregations
  ✅ Better compression
  ⚠️ Less flexible for schema changes
```

**Investigation needed:**
- Benchmark EAV vs columnar in Postgres
- Test with RisingWave (which uses LSM trees)
- Profile with realistic Boon queries

**Hypothesis:** EAV for flexibility, columnar for analytics (use both?).

---

**3. Content-Addressing in Practice**

**Question:** How to implement content-addressed types in Boon's compiler?

**Challenges:**
- Hash collision handling (SHA3-512 like Unison?)
- Incremental hashing (re-hash on every edit?)
- Hash storage format (where to store #hash_type_v1?)

**Investigation needed:**
- Study Unison's hashing algorithm
- Design Boon's type hash format
- Integrate with triple store

**Open design question:** Should Boon hash at compile-time or runtime?

---

**4. Abilities System Design**

**Question:** Exact syntax and semantics for Boon's abilities?

**Options:**
```boon
-- Option A: Unison-style (curly braces)
process: Input -> {IO, Store} Output

-- Option B: Pipe-style (more Boon-like?)
process: Input |> {IO, Store} |> Output

-- Option C: Attribute-style
#[abilities(IO, Store)]
process: Input -> Output
```

**Investigation needed:**
- Design handler syntax
- Type system integration
- How handlers compose

**Current preference:** Option A (follows Unison, well-understood).

---

**5. Distributed Code Deployment**

**Question:** How to serialize and deploy Boon code by hash to cluster nodes?

**Challenges:**
- Code serialization format (AST? Bytecode? WASM?)
- Dependency resolution (recursive hash fetching)
- Caching strategy (how long to cache #hash_fn?)
- Version conflicts (ensure same hash → same behavior)

**Investigation needed:**
- Design code serialization format
- Build hash-based code cache
- Test with actual distributed execution

**Inspiration:** Unison's content-addressed deployment (ask for missing definitions by hash).

---

**6. Cell-Level Reactivity Implementation**

**Question:** How exactly does ElectricSQL achieve cell-level reactivity with triples?

**Current understanding:**
```sql
-- ElectricSQL shape (subscribe to specific triple patterns)
SELECT value FROM triples
WHERE entity_id = 'user_1'
  AND attribute = 'email';

-- Only notified when user_1's email changes!
-- NOT when user_1's name changes!
```

**Questions:**
- How to map Boon QUERY to ElectricSQL shapes?
- Performance with millions of shapes?
- Can we batch shape subscriptions?

**Investigation needed:**
- Deep dive into ElectricSQL shape implementation
- Prototype triple store with ElectricSQL
- Benchmark reactivity performance

---

**7. NATS vs S3 for BYTES**

**Question:** When to use NATS Object Store vs S3 for large blobs?

**Trade-offs:**
```
NATS Object Store:
  ✅ Distributed (built-in clustering)
  ✅ Real-time sync
  ✅ Works across browser/server
  ⚠️ Storage limits? (need to check)
  ⚠️ Cost? (less mature than S3)

S3:
  ✅ Unlimited storage
  ✅ Extremely cheap
  ✅ Battle-tested at scale
  ⚠️ Not real-time (polling needed)
  ⚠️ Browser access needs CORS/auth
```

**Hypothesis:** Use both!
- NATS for real-time sync (metadata + small files)
- S3 for archival (large files)

**Investigation needed:**
- Test NATS Object Store limits
- Design hybrid NATS+S3 architecture

---

**8. Hardware Synthesis**

**Question:** How to compile Boon TABLE/RELATION to hardware (Verilog/VHDL)?

**Challenges:**
- CAM synthesis (not all FPGAs have CAM primitives)
- Hash function in hardware (SHA3 too expensive?)
- Size limits (how much BRAM available?)

**Investigation needed:**
- Research hardware CAM implementations
- Pick lightweight hash for hardware (CRC? MurmurHash?)
- Test on actual FPGA

**Fallback:** Start with software/server, add hardware later.

---

## Potential Pitfalls

### 1. BSL License Trap (Materialize)

**Pitfall:** Materialize looks perfect, but BSL license has restrictions!

**Problem:**
- ❌ Cannot offer clustered deployment as commercial service
- ❌ Cannot self-host multi-node cluster for production
- ⏰ 4-year wait for Apache 2.0 conversion

**Solution:**
- ✅ Use Materialize as **inspiration only**
- ✅ Use Differential Dataflow (MIT) directly
- ✅ Or use RisingWave (Apache 2.0)

**Lesson:** Always check licenses! Source-available ≠ Open source.

---

### 2. DBSP Distributed Runtime Not Ready

**Pitfall:** DBSP is amazing but distributed runtime still in development!

**Problem:**
- ✅ Single-node works great
- 🚧 Multi-node architecture designed but not implemented
- ⏰ No timeline for distributed release

**Solution:**
- **Option A:** Wait for DBSP distributed (best long-term)
- **Option B:** Use RisingWave now (distributed ready)
- **Option C:** Start with Differential Dataflow (distributed works)

**Lesson:** Check production readiness, not just features!

---

### 3. EAV Table Performance

**Pitfall:** Triple store (EAV) can be slow for complex queries!

**Problem:**
```sql
-- Query: Get user's name, email, and age
-- Requires 3 self-joins in EAV!
SELECT
  n.value AS name,
  e.value AS email,
  a.value AS age
FROM triples n
JOIN triples e ON n.entity = e.entity
JOIN triples a ON n.entity = a.entity
WHERE n.entity = 'user_1'
  AND n.attribute = 'name'
  AND e.attribute = 'email'
  AND a.attribute = 'age';

-- 3-way self-join! Potentially slow!
```

**Solution:**
- **Indexes:** Index on (entity, attribute, value)
- **Denormalization:** Cache common queries
- **Hybrid:** Use EAV for triples, columnar for aggregations
- **Incremental:** DBSP makes this fast (only delta computed!)

**Lesson:** Test performance early with realistic data!

---

### 4. Content-Addressing Overhead

**Pitfall:** Hashing everything might be slow!

**Problem:**
- Hash computation cost (SHA3-512 is expensive)
- Storage overhead (store hashes + names + definitions)
- Cache invalidation (when to recompute hashes?)

**Solution:**
- **Incremental hashing:** Only hash changed definitions
- **Cache aggressively:** Hash → AST mapping cached forever
- **Lazy hashing:** Only hash when needed (not every keystroke)

**Inspiration:** Unison caches hashes, only computes on "add" command.

**Lesson:** Content-addressing is powerful but needs careful caching!

---

### 5. Distributed Debugging Nightmare

**Pitfall:** Debugging distributed incremental computation is HARD!

**Problem:**
```
User: "Why is this query slow?"
Dev: "Let me check..."
  - Is it the triple store? (network latency?)
  - Is it the incremental engine? (too many deltas?)
  - Is it the sync layer? (ElectricSQL buffering?)
  - Is it the handler? (slow Postgres query?)
  - Which node is the bottleneck?
```

**Solution:**
- **Tracing:** OpenTelemetry for distributed traces
- **Metrics:** Prometheus for performance monitoring
- **Logging:** Structured logs (JSON) with correlation IDs
- **Profiling:** Per-node profiling (flame graphs)

**Lesson:** Build observability from day 1!

---

### 6. Schema Evolution Complexity

**Pitfall:** Multiple versions coexisting is powerful but confusing!

**Problem:**
```boon
// User v1
[user_1, type, #hash_User_v1]
[user_1, name, TEXT { Alice }]

// User v2
[user_2, type, #hash_User_v2]
[user_2, name, TEXT { Bob }]
[user_2, email, TEXT { bob@... }]

// Query: Get ALL users
// Which fields do they have? (v1 has no email!)
// How to handle missing fields?
```

**Solution:**
- **Optional fields:** Every field is `Option<T>` (nullable)
- **Explicit versions:** Query knows which version
- **Migration helpers:** Functions to migrate v1 → v2

**Lesson:** Schema evolution is easy to add attributes, hard to make them required!

---

### 7. WASM Performance Limitations

**Pitfall:** WASM might be too slow for heavy incremental computation!

**Problem:**
- No SIMD (yet) in stable WASM
- No threads (without SharedArrayBuffer)
- Slower than native (10-50% overhead)

**Solution:**
- **Benchmark early:** Test Differential Dataflow in WASM
- **Offload heavy work:** Send to server if too slow
- **Web Workers:** Use for parallelism
- **Wait for WASM proposals:** SIMD, threads coming

**Fallback:** Heavy computation on server, light queries in browser.

**Lesson:** WASM is great but not a silver bullet!

---

### 8. Dependency Hell (Even with Hashing!)

**Pitfall:** Content-addressing doesn't eliminate ALL dependency problems!

**Problem:**
```boon
// Module A uses function #hash_fn_v1
// Module B uses function #hash_fn_v2
// Both try to serialize same data type
// Serialization format incompatible!
// Content hashes match but behavior differs!
```

**Solution:**
- **Hash everything:** Types, serialization format, everything
- **Version tags:** Explicit versioning alongside hashes
- **Type system enforcement:** Compiler catches mismatches

**Lesson:** Content-addressing helps a LOT, but careful design still needed!

---

## Domain-Specific Comparison

### Hardware Domain

**Technology Stack:**
```
Language:     Boon (compiles to Verilog/VHDL)
Storage:      BRAM (Block RAM)
Lookup:       CAM (Content-Addressable Memory) or Hash+BRAM
Queries:      Datafrog (simple, fast) or custom
Incremental:  NOT NEEDED (hardware fast enough for full recomputation)
Sync:         N/A (hardware doesn't sync)
```

**Design Decisions:**

| Aspect | Choice | Rationale |
|--------|--------|-----------|
| **Size limits** | Fixed (e.g., 256 entries) | BRAM is limited |
| **Hash function** | CRC32 or MurmurHash3 | SHA3 too expensive in hardware |
| **Lookup latency** | 1-5 cycles (CAM) or 3-10 cycles (Hash+BRAM) | Parallel vs sequential |
| **Power consumption** | CAM higher, Hash+BRAM lower | Trade-off: speed vs power |

**Example:**
```boon
#[hardware]
#[size(256)]
#[hash(crc32)]
opcodes: TABLE { OpCode, Handler }

// Compiles to:
// - 256-entry BRAM for data
// - CRC32 hash function
// - Hash+BRAM lookup (3-5 cycles)
```

**Constraints:**
- ⚠️ **Fixed size:** Cannot grow dynamically (BRAM fixed)
- ⚠️ **No incremental:** Full recomputation on each clock cycle
- ⚠️ **No persistence:** Lost on power-off (unless external flash)

**Benefits:**
- ✅ **Extremely fast:** Nanosecond latency
- ✅ **Parallel:** CAM searches all entries simultaneously
- ✅ **Deterministic:** No GC, no OS, predictable timing

---

### Software Domain (Browser/Desktop)

**Technology Stack:**
```
Language:          Boon (compiles to WASM)
Incremental:       Differential Dataflow (Rust → WASM)
Storage:           IndexedDB (browser) or SQLite (desktop)
Distributed:       NATS (KV + Object Store)
Sync:              ElectricSQL shapes (if server involved)
Large blobs:       NATS Object Store (chunked streaming)
```

**Design Decisions:**

| Aspect | Choice | Rationale |
|--------|--------|-----------|
| **Incremental engine** | Differential Dataflow | WASM-compatible, MIT license |
| **Local storage** | IndexedDB | Standard browser API, async |
| **Offline support** | NATS + local cache | Works without network |
| **Reactivity** | ElectricSQL shapes | Cell-level updates |
| **Large files** | NATS Object Store | Chunked, streamable |

**Example:**
```boon
// Browser app
todos: TABLE { TodoId, Todo }
    |> Table/persist(IndexedDB { db: TEXT { todos_db } })
    |> Table/sync(NATS {
        server: TEXT { nats://demo.nats.io }
        kv_bucket: TEXT { boon_todos }
    })

// Incremental query (Differential Dataflow in WASM)
active_todos: todos
    |> QUERY {
        ?todo WHERE [?todo, status, TEXT { active }]
        SELECT todo
    }
```

**Architecture:**
```
Boon Code
    ↓ (compile)
WASM Module
    ↓ (runs in browser)
Differential Dataflow (incremental queries)
    ↓ (storage)
IndexedDB (local)
    ↓ (sync)
NATS (distributed)
    ↓ (optional server sync)
ElectricSQL (if Postgres backend)
```

**Constraints:**
- ⚠️ **Storage limits:** IndexedDB typically 50-100 MB (browser-dependent)
- ⚠️ **WASM overhead:** 10-50% slower than native
- ⚠️ **No threads:** Unless SharedArrayBuffer (security restrictions)

**Benefits:**
- ✅ **Offline-first:** Works without network
- ✅ **Reactive:** Differential Dataflow updates incrementally
- ✅ **Distributed:** NATS syncs across devices
- ✅ **Cross-platform:** WASM runs everywhere

---

### Server Domain

**Technology Stack:**
```
Language:          Boon (compiles to Rust or runs on VM)
Incremental:       DBSP/Feldera (when distributed) or RisingWave
Storage:           Postgres (EAV triples) + S3 (large blobs)
Distributed:       RisingWave cluster or Arroyo (Kubernetes)
Sync:              ElectricSQL (Postgres logical replication)
Large blobs:       S3 (primary) + NATS Object Store (real-time)
Stream processing: Arroyo (if Kafka/streaming needed)
```

**Design Decisions:**

| Aspect | Choice | Rationale |
|--------|--------|-----------|
| **Incremental engine** | RisingWave or DBSP (when ready) | Distributed, production-ready |
| **Triple store** | Postgres EAV table | Battle-tested, logical replication |
| **Large blobs** | S3 | Unlimited, cheap, durable |
| **Distributed** | RisingWave cluster (Kubernetes) | Elastic scaling, HA |
| **Sync to clients** | ElectricSQL | Cell-level reactivity |
| **Stream processing** | Arroyo (if needed) | Kafka, real-time pipelines |

**Example:**
```boon
// Server app
users: TABLE { UserId, User }
    |> Table/backend(
        storage: Backend/risingwave(
            postgres_url: Env/var(TEXT { DATABASE_URL })
            s3_bucket: TEXT { my-boon-storage }
        )
    )
    |> Table/sync(
        electric: Backend/electric_sql(
            url: Env/var(TEXT { DATABASE_URL })
            sync_granularity: Cell  // Field-level!
        )
    )

// Large file storage
videos: TABLE { VideoId, Video }
    |> Table/blobs(
        storage: Backend/s3(
            bucket: TEXT { my-boon-videos }
            region: TEXT { us-west-2 }
        )
    )
```

**Architecture:**
```
Boon Code
    ↓ (compile)
RisingWave Cluster (Kubernetes)
    ↓ (triple store)
Postgres (EAV table) + S3 (blobs)
    ↓ (logical replication)
ElectricSQL (sync layer)
    ↓ (push updates)
Browser clients (Differential Dataflow in WASM)
```

**Postgres EAV Schema:**
```sql
CREATE TABLE triples (
    entity_id  UUID,
    attribute  TEXT,
    value      JSONB,
    type_hash  TEXT,  -- #hash_Type_v1
    timestamp  TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (entity_id, attribute)
);

CREATE INDEX idx_triples_entity ON triples (entity_id);
CREATE INDEX idx_triples_attribute ON triples (attribute);
CREATE INDEX idx_triples_value ON triples USING GIN (value);
```

**S3 Blob Storage:**
```
s3://my-boon-storage/
  entities/
    user_1/
      avatar.jpg  (metadata: {type_hash: #Image_v1, size: 1024000})
    video_1/
      content.mp4  (metadata: {type_hash: #Video_v1, size: 104857600})
```

**Constraints:**
- ⚠️ **Cost:** S3 + Postgres + compute (monitor spending!)
- ⚠️ **Complexity:** More moving parts (Kubernetes, ElectricSQL, etc.)
- ⚠️ **Latency:** Network hops add latency vs local

**Benefits:**
- ✅ **Scalable:** RisingWave elastic (handles millions of events/sec)
- ✅ **Durable:** S3 + Postgres = battle-tested
- ✅ **Distributed:** Multi-node HA
- ✅ **Real-time sync:** ElectricSQL pushes updates to clients

---

## Cross-Domain Integration

### How All Three Domains Work Together

**Example: IoT Sensor Data Pipeline**

```
HARDWARE (FPGA sensor):
  - Collect sensor readings (temperature, pressure)
  - Store in BRAM (256 recent readings)
  - Hash + CAM lookup for anomaly detection
  - Send alerts via UART/network

     ↓ (network)

SOFTWARE (Browser dashboard):
  - Receive sensor data via NATS
  - Store in IndexedDB (local cache)
  - Differential Dataflow for queries
  - Display real-time charts

     ↓ (sync via ElectricSQL)

SERVER (Data warehouse):
  - RisingWave cluster processes all sensors
  - Postgres stores triples (historical data)
  - S3 stores raw sensor dumps (blobs)
  - ElectricSQL syncs to browsers
```

**Unified Boon Code:**
```boon
// Define sensor data structure
SensorReading : TYPE {
    sensor_id: SensorId
    temperature: Float
    pressure: Float
    timestamp: Time
}

// HARDWARE: Collect and detect anomalies
#[hardware]
#[size(256)]
sensor_buffer: TABLE { timestamp: Time, SensorReading }

#[hardware]
anomaly_detected: sensor_buffer
    |> Table/filter(reading: reading.temperature > 100.0)
    |> Table/map(reading: Alert { sensor: reading.sensor_id })

// SOFTWARE: Dashboard query
#[software]
recent_readings: sensor_buffer
    |> Table/sync_from(NATS { ... })
    |> QUERY {
        ?reading WHERE [?reading, timestamp, ?ts]
        ?ts > (now() - hours(1))
        SELECT reading
    }

// SERVER: Historical analysis
#[server]
all_readings: TABLE { ReadingId, SensorReading }
    |> Table/backend(RisingWave { ... })

#[server]
daily_averages: all_readings
    |> QUERY {
        ?reading WHERE [?reading, timestamp, ?ts]
        GROUP BY day(?ts)
        SELECT { day, avg(temperature), avg(pressure) }
    }
```

**Key Integration Points:**

1. **NATS for messaging:** Hardware → Software → Server
2. **Same type definitions:** `SensorReading` used everywhere
3. **Different handlers:** Storage backend swapped per domain
4. **Unified queries:** Same QUERY syntax, different engines

---

## Recommended Implementation Order

### Phase 1: Prototype Triple Store (Software Domain)

**Goal:** Validate core concepts in browser

**Tasks:**
1. Implement simple triple store (in-memory)
2. Integrate Differential Dataflow (WASM)
3. Test incremental queries
4. Benchmark performance

**Success criteria:**
- ✅ Triples stored and queried
- ✅ Incremental updates working
- ✅ WASM compiles and runs
- ✅ Performance acceptable (< 100ms for typical queries)

**Tech stack:**
- Rust → WASM
- Differential Dataflow
- IndexedDB (optional)

**Estimated time:** 2-4 weeks

---

### Phase 2: Server Backend (RisingWave)

**Goal:** Production server with S3 storage

**Tasks:**
1. Set up RisingWave cluster (Kubernetes or single node)
2. Create Postgres EAV schema
3. Integrate S3 for blobs
4. Test sync with ElectricSQL

**Success criteria:**
- ✅ Triples in Postgres
- ✅ Blobs in S3
- ✅ ElectricSQL syncing
- ✅ Cell-level reactivity working

**Tech stack:**
- RisingWave (Apache 2.0)
- Postgres + S3
- ElectricSQL

**Estimated time:** 3-6 weeks

---

### Phase 3: Content-Addressed Types

**Goal:** Implement Unison-inspired content addressing

**Tasks:**
1. Design type hashing (SHA3-512?)
2. Store type hashes in triple store
3. Implement version coexistence
4. Test schema evolution

**Success criteria:**
- ✅ Types hashed correctly
- ✅ Multiple versions coexist
- ✅ No migrations needed
- ✅ Deserialize with correct version

**Tech stack:**
- SHA3 hashing (Rust)
- Triple store integration

**Estimated time:** 2-3 weeks

---

### Phase 4: Abilities System

**Goal:** Backend-agnostic effects

**Tasks:**
1. Design ability syntax
2. Implement handler mechanism
3. Create standard abilities (IO, Store, Remote)
4. Test with different handlers (browser vs server)

**Success criteria:**
- ✅ Abilities in type signatures
- ✅ Handlers swap backends
- ✅ Same code runs in browser/server
- ✅ Testable (mock handlers)

**Tech stack:**
- Boon compiler integration
- Handler runtime

**Estimated time:** 4-6 weeks

---

### Phase 5: Distributed Deployment

**Goal:** Content-addressed code distribution

**Tasks:**
1. Implement code hashing
2. Build hash-based cache
3. Test distributed execution (cluster)
4. Benchmark vs traditional deployment

**Success criteria:**
- ✅ Code serialized by hash
- ✅ Nodes fetch missing code
- ✅ Cache working (no re-fetching)
- ✅ Performance comparable to native

**Tech stack:**
- Content-addressing
- NATS or custom protocol
- RisingWave cluster

**Estimated time:** 6-8 weeks

---

### Phase 6: Hardware Synthesis (Optional)

**Goal:** Compile Boon to hardware

**Tasks:**
1. Design BRAM/CAM synthesis
2. Implement hash function in hardware
3. Generate Verilog/VHDL
4. Test on FPGA

**Success criteria:**
- ✅ Boon TABLE compiles to Verilog
- ✅ Runs on FPGA
- ✅ Performance meets requirements
- ✅ Resource usage acceptable

**Tech stack:**
- Verilog/VHDL generation
- FPGA toolchain (Vivado, Quartus)

**Estimated time:** 8-12 weeks (complex!)

---

## Summary: The Path Forward

**Immediate Next Steps:**
1. ✅ Research complete (6,696 lines documented!)
2. 🎯 **Prototype triple store** (Phase 1)
3. 🎯 **Integrate RisingWave** (Phase 2)
4. 🎯 **Add content-addressing** (Phase 3)

**Key Technologies:**
- **Rust:** Everything in Rust (WASM-compatible)
- **Differential Dataflow:** Software domain (MIT license)
- **RisingWave:** Server domain (Apache 2.0, S3-native)
- **ElectricSQL:** Cell-level sync (Postgres logical replication)
- **NATS:** Distributed messaging (KV + Object Store)

**Avoid:**
- ❌ Materialize (BSL license restrictions)
- ❌ Datafrog for large datasets (no incremental)
- ❌ Premature optimization (prototype first!)

**Watch For:**
- ⚠️ DBSP distributed runtime (when ready, switch to it)
- ⚠️ WASM performance (benchmark early)
- ⚠️ EAV performance (test with realistic data)

**End Goal:**
- Reactive (incremental computation, 100,000x faster)
- Persistent (S3 + Postgres + IndexedDB)
- Distributed (NATS + RisingWave + ElectricSQL)
- Schema-less (content-addressed types, no migrations)
- Cross-domain (hardware/software/server unified)

**The future of Boon's data layer is clear: Triple Store + Datalog + Incremental Computation + Content-Addressing!**
