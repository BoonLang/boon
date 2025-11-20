# Rust Streaming & Incremental Computation Ecosystem

This document surveys the Rust ecosystem for streaming dataflow, incremental computation, and distributed stream processing - all highly relevant for Boon's storage layer design.

---

## Core Foundations

### Timely Dataflow

**Repository:** https://github.com/TimelyDataflow/timely-dataflow
**Creator:** Frank McSherry
**Status:** Production-ready (v0.25.1, October 2025)
**Stars:** 3.5k | **Downloads:** 13,364/month

**What It Is:**

Timely Dataflow is the foundational low-latency cyclic dataflow computational model in Rust. It's a distributed data-parallel compute engine that scales from single-threaded execution on a laptop to distributed execution across clusters.

**Key Features:**

- **Primitive operators:** `map`, `filter`, `enter`, `leave` (for loops)
- **Generic operators:** `unary`, `binary` for custom closures
- **Multi-threaded & distributed:** Configurable worker processes
- **Progress tracking:** Probes for monitoring computation completion
- **Exchange channels:** Custom data routing between workers

**Architecture:**

```rust
// Basic timely dataflow example
use timely::dataflow::operators::{ToStream, Inspect};

timely::example(|scope| {
    (0..10).to_stream(scope)
        .inspect(|x| println!("seen: {:?}", x));
});
```

**Use Cases:**

- Stream processing and data-parallel computations
- Complex joins (worst-case optimal joins)
- Graph algorithms (PageRank)
- Iterative algorithms with loop support

**Relationship to Others:**

Timely Dataflow is the **foundation** for:
- Differential Dataflow (incremental computation)
- Materialize (streaming SQL database)
- Various research projects

---

### Differential Dataflow

**Repository:** https://github.com/TimelyDataflow/differential-dataflow
**Creator:** Frank McSherry
**Status:** Production-ready
**Based on:** Timely Dataflow

**What It Is:**

Differential Dataflow extends Timely Dataflow with **incremental computation**. Instead of recomputing entire results when data changes, it computes only the delta (difference).

**Revolutionary Performance:**

```
Graph degree computation (1M nodes):
- Initial query: 72 seconds
- Single edge update: 0.5 milliseconds
= 100,000x speedup!
```

**Core Concept:**

Data as `(record, diff, time)` triples:

```rust
(User{id: 1, name: "Alice"}, +1, t=0)  // Added
(User{id: 1, name: "Alice"}, -1, t=1)  // Removed
(User{id: 1, name: "Alicia"}, +1, t=1) // New version
```

**Operators:**

- `group` - Incremental grouping
- `join` - Incremental joins
- `iterate` - Incremental fixed-point iteration
- `reduce` - Incremental aggregation

**Example:**

```rust
use differential_dataflow::operators::*;

// Incremental join
let query = users
    .join(&posts.map(|p| (p.author_id, p)))
    .map(|(user_id, (user, post))| (user.name, post.title));

// Add one post → only that post processed!
// Not the entire join recomputed!
```

**Used By:**

- Materialize (streaming SQL database)
- Differential Datalog (DDlog)
- 3DF (reactive Datalog)
- Feldera/DBSP

---

## High-Level Languages & Systems

### Differential Datalog (DDlog)

**Repository:** https://github.com/vmware-archive/differential-datalog
**Creator:** VMware Research (now archived)
**Status:** Archived (still usable)
**License:** MIT

**What It Is:**

A **programming language** for incremental computation that compiles to Differential Dataflow/Rust.

**Key Features:**

- **Declarative Datalog syntax** (not imperative)
- **Powerful type system:** Booleans, unlimited precision integers, bitvectors, floats, strings, tuples, tagged unions, vectors, sets, maps
- **Automatic incrementalization:** Compiler generates efficient incremental code
- **Multi-language bindings:** Rust, C/C++, Java, Go

**Example:**

```datalog
// Define relations
input relation User(id: UserId, name: string)
input relation Post(id: PostId, author: UserId, title: string)

// Query (automatically incremental!)
output relation UserPosts(user_name: string, post_title: string)

UserPosts(user_name, post_title) :-
    User(user_id, user_name),
    Post(_, user_id, post_title).

// When one post added → only that post processed!
// Compiler generates incremental Differential Dataflow code!
```

**Benefits:**

- Write declarative logic, get incremental computation for free
- No manual index management
- Compiled to Rust (fast!)

**Drawbacks:**

- Archived project (VMware discontinued)
- Less actively maintained than alternatives

---

### DBSP (Feldera)

**Repository:** https://github.com/feldera/feldera
**Paper:** "DBSP: Automatic Incremental View Maintenance" (VLDB 2023 Best Paper)
**Status:** Production (v0.x, active development)

**What It Is:**

Database-grade incremental query engine that compiles **SQL and Datalog** to incremental operators.

**Key Innovation:**

DBSP moves from "developer framework" (Timely/Differential) to **"database-grade, verifiable engine"** with formal verification to prevent race conditions.

**Architecture:**

```
SQL/Datalog Query
      ↓
DBSP Compiler (optimizes to circuit)
      ↓
DBSP Runtime (Rust, formally verified)
      ↓
Incremental Updates (100,000x faster)
```

**Advantages over Differential Dataflow:**

- **SQL support** (not just Rust API)
- **Query optimizer** (chooses best incremental strategy)
- **Formal verification** (provably correct)
- **Enterprise-grade** (Feldera company provides support)

**Example:**

```sql
-- Standard SQL (automatically incremental!)
CREATE MATERIALIZED VIEW user_posts AS
SELECT u.name, p.title
FROM users u
JOIN posts p ON u.id = p.author_id;

-- Add one post → incremental update (0.5ms, not 72s!)
```

**For Boon:**

DBSP is the **most mature, production-ready** option for compiling queries to incremental computation. Used by Feldera cloud platform.

---

## Streaming Databases

### Materialize

**Repository:** https://github.com/MaterializeInc/materialize
**Website:** https://materialize.com
**Status:** Production (cloud service, open-source core)
**Based on:** Differential Dataflow

**What It Is:**

Streaming SQL database that uses **Differential Dataflow** under the hood. Looks like Postgres, acts like Postgres, but maintains **incremental materialized views**.

**Key Features:**

- **Standard SQL** (Postgres-compatible)
- **Incremental views:** Updates in milliseconds, not hours
- **Real-time queries:** Subscribe to changes (SUBSCRIBE syntax)
- **Source connectors:** Kafka, Postgres CDC, webhooks

**Architecture:**

```
Kafka/Postgres → Materialize (Differential Dataflow)
                      ↓
              Materialized Views (incremental!)
                      ↓
              Postgres Wire Protocol (queries/subscriptions)
```

**Example:**

```sql
-- Connect to Kafka
CREATE SOURCE user_events
FROM KAFKA BROKER 'localhost:9092'
TOPIC 'users';

-- Incremental materialized view
CREATE MATERIALIZED VIEW user_summary AS
SELECT user_id, COUNT(*) as event_count
FROM user_events
GROUP BY user_id;

-- Real-time subscription!
SUBSCRIBE TO user_summary;
-- Pushes updates when user_summary changes!
```

**Founded by:** Frank McSherry (Differential Dataflow creator)

**For Boon:**

Materialize proves Differential Dataflow works at **production scale** for streaming SQL. $100M+ funding, used by enterprises.

---

### RisingWave

**Repository:** https://github.com/risingwavelabs/risingwave
**Website:** https://risingwave.com
**Status:** Production (v2.x)
**Language:** Rust

**What It Is:**

Cloud-native streaming database, Postgres-compatible, with **incremental materialized views**. Built from scratch in Rust (not on Differential Dataflow).

**Key Features:**

- **Postgres-compatible SQL**
- **Incremental computation** (custom implementation, not DD)
- **Change propagation framework** for materialized views
- **Log-structured merge trees** (LSM) for storage
- **No JVM overhead** (pure Rust)

**Architecture:**

```
Kafka/CDC → RisingWave (Rust incremental engine)
                ↓
        Materialized Views (LSM trees)
                ↓
        Postgres Protocol (queries)
```

**Comparison to Materialize:**

| Feature | Materialize | RisingWave |
|---------|-------------|------------|
| Foundation | Differential Dataflow | Custom Rust |
| Storage | In-memory + S3 | LSM trees |
| Performance | Extremely fast updates | Fast, optimized for cost |
| Maturity | 2019, Frank McSherry | 2021, newer |

**For Boon:**

RisingWave shows you **don't have to use Differential Dataflow** to build incremental streaming databases, but it requires reimplementing everything.

---

## Stream Processing Engines

### Arroyo

**Repository:** https://github.com/ArroyoSystems/arroyo
**Website:** https://arroyo.dev
**Status:** Production (v0.14.1, June 2025)
**Stars:** 4.6k
**Language:** Rust

**What It Is:**

Distributed stream processing engine in Rust, **serverless/cloud-native**, alternative to Apache Flink.

**Key Features:**

- **SQL & Rust pipelines** (both first-class)
- **Millions of events/sec** throughput
- **Stateful operations:** Windows, joins, aggregations
- **Fault tolerance:** State checkpointing
- **Serverless:** Auto-scaling, cloud-native

**Performance vs Flink:**

```
Sliding window benchmark (1M events):
- Flink: Degrades with small slides + large windows
- Arroyo: Constant performance (10x faster)
```

**Example:**

```sql
-- SQL pipeline in Arroyo
CREATE TABLE users (
  id INT,
  name TEXT,
  event_time TIMESTAMP
) WITH (
  connector = 'kafka',
  topic = 'users'
);

-- Sliding window aggregation
SELECT
  TUMBLE_START(event_time, INTERVAL '1' MINUTE) as window_start,
  COUNT(*) as event_count
FROM users
GROUP BY TUMBLE(event_time, INTERVAL '1' MINUTE);
```

**Architecture:**

Built on **Timely Dataflow** (same as Differential Dataflow), but focuses on **stream processing** (not incremental computation).

**For Boon:**

Arroyo shows Rust can **replace Flink** with better performance and simpler operations. Good for distributed streaming (server domain).

---

### Hydroflow

**Repository:** https://github.com/hydro-project/hydro
**Website:** https://hydro.run
**Creator:** UC Berkeley (Joe Hellerstein)
**Status:** Active research (POPL 2025, SPLASH 2023 papers)
**Language:** Rust

**What It Is:**

Low-level dataflow runtime for distributed systems, intended as **"LLVM IR for distributed programs"**. Part of the Hydro Project at UC Berkeley.

**Key Innovation:**

Hydroflow provides a **compiler** that transforms async Rust functions into dataflow graphs, enabling formal reasoning about distributed systems.

**Architecture:**

```
Async Rust Functions
      ↓
Hydroflow Compiler
      ↓
Dataflow IR (DFIR)
      ↓
Optimized Rust (monomorphization)
      ↓
Extremely low-latency execution
```

**Features:**

- **Formal semantics** (POPL 2025: "Flo: Semantic Foundation for Progressive Stream Processing")
- **Dataflow optimizations** (local rewrites, OOPSLA 2023)
- **Async-to-dataflow compiler** (POPL 2025 SRC)
- **Low-latency** (aggressive monomorphization)

**Example:**

```rust
// Hydroflow dataflow graph
let mut df = hydroflow_syntax! {
    source_iter(0..10)
        -> map(|x| x * 2)
        -> filter(|x| x > 5)
        -> for_each(|x| println!("{}", x));
};

df.run_available();
```

**Research Focus:**

- Correctness guarantees for distributed systems
- Formal verification of dataflow programs
- Compiler optimizations for streaming

**For Boon:**

Hydroflow represents **cutting-edge research** in dataflow systems with formal semantics. Could inspire Boon's correctness guarantees.

---

### Other Rust Stream Processors

**rlink-rs:**
- **Repository:** https://github.com/rlink-rs/rlink-rs
- **Description:** Apache Flink reimplementation in Rust
- **Status:** Less mature than Arroyo

**Bytewax:**
- **Based on:** Timely Dataflow (Rust core)
- **Interface:** Python
- **Description:** Stream processing with Python API, Rust performance

**Pathway:**
- **Core:** Rust
- **Interface:** Python
- **Description:** Streaming ETL, powered by Rust

---

## Specialized Tools

### Datafrog

**Repository:** https://github.com/rust-lang/datafrog
**Creator:** Frank McSherry (for Rust compiler team)
**Status:** Production (used in Rust compiler's Polonius borrow checker)

**What It Is:**

Simplified Datalog engine for **performance-critical** use cases where Differential Dataflow is too complex.

**Key Differences from Differential Dataflow:**

- **No incremental computation** (full recomputation)
- **Manual index management** (you control joins)
- **Extremely fast** for small datasets
- **Simpler** (no timestamps, no diffs)

**Use Case:**

Rust's **Polonius borrow checker** uses Datafrog for analyzing Rust programs. Needs speed, not incrementality.

**Example:**

```rust
use datafrog::{Iteration, Relation};

let mut iteration = Iteration::new();
let variable = iteration.variable("variable");

// Load initial facts
variable.insert(Relation::from(vec![(1, 2), (2, 3)]));

// Fixed-point iteration
while iteration.changed() {
    // Manual join (you control indexes!)
    variable.from_join(&variable, &variable, |&k, &v1, &v2| (k, v2));
}
```

**For Boon:**

Datafrog shows sometimes **simple > complex**. For small datasets (hardware domain?), Datafrog might outperform Differential Dataflow.

---

## Comparison Matrix

| System | Foundation | Incremental | SQL | Distributed | Maturity | Best For |
|--------|-----------|-------------|-----|-------------|----------|----------|
| **Timely Dataflow** | - | ❌ | ❌ | ✅ | Production | Foundation library |
| **Differential Dataflow** | Timely | ✅ | ❌ | ✅ | Production | Incremental computation |
| **DBSP (Feldera)** | Custom | ✅ | ✅ | ✅ | Production | SQL incremental queries |
| **Differential Datalog** | Differential | ✅ | ❌ | ✅ | Archived | Datalog programs |
| **Materialize** | Differential | ✅ | ✅ | ✅ | Production | Streaming SQL database |
| **RisingWave** | Custom | ✅ | ✅ | ✅ | Production | Cloud-native streaming DB |
| **Arroyo** | Timely | ❌ | ✅ | ✅ | Production | Flink replacement |
| **Hydroflow** | Custom | ❌ | ❌ | ✅ | Research | Verified distributed systems |
| **Datafrog** | - | ❌ | ❌ | ❌ | Production | Polonius borrow checker |

---

## Key Insights for Boon

### 1. Timely Dataflow is the Foundation

Almost everything builds on Timely Dataflow:
- Differential Dataflow
- Arroyo
- Materialize (via Differential)

**Implication:** If Boon adopts this ecosystem, start with **Timely Dataflow** understanding.

### 2. Two Paths to Incremental Computation

**Path A: Use Differential Dataflow**
- ✅ Proven 100,000x speedups
- ✅ Production-ready
- ❌ Rust API only (need wrapper)

**Path B: Use DBSP/Feldera**
- ✅ SQL support (compile queries)
- ✅ Query optimizer
- ✅ Formally verified
- ✅ Production-ready

**Recommendation for Boon:** **DBSP/Feldera** is better for database use cases (compiles SQL/Datalog, not just Rust API).

### 3. Streaming Database Proof Points

**Materialize** and **RisingWave** prove Rust incremental computation works at **production scale** for databases:
- Materialize: $100M+ funding, Frank McSherry
- RisingWave: 10x better cost efficiency vs Flink

**Implication:** Boon can confidently build on this foundation.

### 4. Hydroflow for Formal Semantics

UC Berkeley's Hydroflow project provides **formal semantics** for streaming dataflow (POPL 2025).

**Implication:** Could inspire Boon's correctness guarantees and verification.

### 5. Not Everything Needs Differential Dataflow

**Datafrog** (Rust compiler) and **RisingWave** (streaming DB) show you can build high-performance systems **without** Differential Dataflow.

**Implication:** Boon could:
- Use Differential for complex queries (big datasets)
- Use simpler approaches for small datasets (hardware domain)

---

## Architecture Recommendation for Boon

Based on this ecosystem survey, here's the recommended architecture:

```
┌─────────────────────────────────────────────┐
│        Boon Query Language (TABLE, etc.)    │
└──────────────────┬──────────────────────────┘
                   │
                   ↓
┌─────────────────────────────────────────────┐
│         Query Compiler (Boon → SQL/Datalog) │
└──────────────────┬──────────────────────────┘
                   │
                   ↓
┌─────────────────────────────────────────────┐
│      DBSP/Feldera (SQL/Datalog → Incremental)│
│         or Differential Dataflow            │
└──────────────────┬──────────────────────────┘
                   │
    ┌──────────────┼──────────────┐
    ↓              ↓              ↓
┌────────┐   ┌──────────┐   ┌──────────┐
│Hardware│   │ Software │   │  Server  │
│Datafrog│   │Differential│  │  DBSP   │
│(small) │   │ Dataflow │   │(Postgres)│
└────────┘   └──────────┘   └──────────┘
```

**Hardware Domain (small datasets):**
- Use **Datafrog** (simple, fast, no incremental overhead)
- Or direct CAM/BRAM implementations

**Software Domain (browser):**
- Use **Differential Dataflow** (incremental, WASM-compatible)
- Or DBSP if it compiles to WASM

**Server Domain (database):**
- Use **DBSP/Feldera** (SQL support, query optimization)
- With Postgres backend (EAV triple store)
- ElectricSQL for sync

---

## Critical Production Considerations

### Open Source Licenses

| System | License | Fully Open Source? | Restrictions |
|--------|---------|-------------------|--------------|
| **Timely Dataflow** | MIT | ✅ YES | None - permissive |
| **Differential Dataflow** | MIT | ✅ YES | None - permissive |
| **DBSP Core** | MIT | ✅ YES | None - permissive |
| **DBSP SQL Compiler** | Apache 2.0 | ✅ YES | None - permissive |
| **Feldera** | MIT | ✅ YES | None - permissive |
| **Differential Datalog** | MIT | ✅ YES | VMware archived, but usable |
| **Materialize** | BSL 1.1 | ⚠️ NO | Source-available, not OSS |
| **RisingWave** | Apache 2.0 | ✅ YES | None - permissive |
| **Arroyo** | Apache 2.0 | ✅ YES | None - permissive |
| **Hydroflow** | Apache 2.0 | ✅ YES | None - permissive |
| **Datafrog** | MIT/Apache 2.0 | ✅ YES | None - dual licensed |

**Critical Note on Materialize:**

Materialize uses the **Business Source License (BSL 1.1)**, which means:
- ❌ **NOT fully open source** (source-available)
- ⚠️ **Clustered deployment restrictions** for commercial use
- ⏰ **Converts to Apache 2.0 after 4 years**
- ❌ **Cannot offer as commercial DBaaS**
- ✅ **Can use non-clustered instances for production**

**For Boon:** Use Materialize as **inspiration** but rely on **Differential Dataflow** (MIT) or **DBSP/Feldera** (MIT/Apache) for actual implementation.

### Language: 100% Rust

| System | Language | Notes |
|--------|----------|-------|
| **Timely Dataflow** | ✅ Rust | Pure Rust |
| **Differential Dataflow** | ✅ Rust | Pure Rust |
| **DBSP/Feldera** | ✅ Rust | Pure Rust |
| **Materialize** | ✅ Rust | Pure Rust (built on Differential) |
| **RisingWave** | ✅ Rust | Pure Rust |
| **Arroyo** | ✅ Rust | Pure Rust |
| **Hydroflow** | ✅ Rust | Pure Rust |
| **Datafrog** | ✅ Rust | Pure Rust |
| **Differential Datalog** | Haskell → Rust | Compiler in Haskell, generates Rust code |

**All major systems are written in Rust!** This ensures:
- Memory safety
- High performance
- Excellent FFI for Boon (if Boon uses Rust internally)
- WASM compilation potential

### Cluster & Distributed Capabilities

| System | Distributed | Cluster Support | HA (High Availability) | Notes |
|--------|-------------|-----------------|----------------------|-------|
| **Timely Dataflow** | ✅ YES | Multi-node | ✅ | Foundation for distributed dataflow |
| **Differential Dataflow** | ✅ YES | Multi-node | ✅ | Builds on Timely's distribution |
| **DBSP/Feldera** | ✅ YES | In development | 🚧 | Design docs published, implementation ongoing |
| **Materialize** | ✅ YES | Multi-active replication | ✅ | Cloud-native, HA via replication |
| **RisingWave** | ✅ YES | Designed distributed from scratch | ✅ | Elastic scaling, compute/storage separation |
| **Arroyo** | ✅ YES | Kubernetes-native | ✅ | Serverless, auto-scaling |
| **Hydroflow** | ✅ YES | Multi-node research | 🔬 | UC Berkeley research project |
| **Datafrog** | ❌ NO | Single-node only | ❌ | Designed for Rust compiler (single-threaded) |

**Key Findings:**

**RisingWave** - Best distributed architecture:
- Designed as distributed system from day 1
- Elastic scaling (separate compute/storage)
- All data in S3, compute scales dynamically
- Production-ready HA

**Arroyo** - Best for Kubernetes:
- Native Kubernetes scheduler
- Serverless auto-scaling
- Fault tolerance via checkpointing

**Feldera** - Distributed coming:
- Currently data-parallel (multi-thread, single node)
- Distributed runtime in active development
- Design docs show multi-node architecture planned

**Materialize** - Production HA:
- Multi-active replication
- High availability in cloud service
- BUT: BSL restrictions on self-hosted clusters

**For Boon Server Domain:**
- **RisingWave** or **Arroyo** for production distributed deployments
- **Feldera** when distributed runtime releases (best for SQL/Datalog)

### File & Blob Storage Support

| System | S3 Support | Object Storage | Large Files | Use Case |
|--------|-----------|----------------|-------------|----------|
| **Feldera** | ✅ YES | Delta Lake, Iceberg | ✅ YES | Connector architecture |
| **Materialize** | ✅ YES | S3 (Persist system) | ✅ YES | Storage layer |
| **RisingWave** | ✅✅✅ YES | **S3 as PRIMARY storage** | ✅ YES | Cloud-native architecture |
| **Arroyo** | ✅ YES | S3, GCS, local FS | ✅ YES | FileSystem connector |
| **Differential Dataflow** | ⚠️ No built-in | Custom possible | ⚠️ | Application-level |
| **Timely Dataflow** | ⚠️ No built-in | Custom possible | ⚠️ | Application-level |

**Detailed Capabilities:**

#### Feldera (DBSP)

**Input Connectors:**
- ✅ Delta Lake (S3, GCS, Azure Blob)
- ✅ Apache Iceberg (S3, GCS, local FS)
- ✅ S3 direct via Delta/Iceberg connectors
- ✅ Read from data lakes

**Storage Strategy:**
- Stores minimal data for incremental computation
- Can spill to NVMe disk when exceeding RAM
- Focus on **computation**, not storage

**Example:**
```sql
CREATE TABLE users WITH (
  'connector' = 'delta_lake_input',
  'uri' = 's3://my-bucket/delta/users/',
  'mode' = 'snapshot_and_follow'
);
```

#### RisingWave - Best S3 Integration

**Hummock State Store:**
- ✅ **S3 as PRIMARY storage backend**
- ✅ All materialized views → S3 blobs
- ✅ Intermediate state → S3
- ✅ Operator outputs → S3
- ✅ Separation of compute/storage

**Object Store Optimizations:**
- Multipart upload (16 MB parts optimal)
- Hybrid caching (Foyer library in Rust)
- Handles SST files up to 512 MiB

**Sinking to Object Storage:**
- ✅ S3, GCS, Azure Blob, WebHDFS
- ✅ Parquet or JSON formats
- ✅ Batching strategies (prevents small files)

**Example:**
```sql
CREATE SINK user_export AS
SELECT * FROM users
WITH (
  connector = 's3',
  s3.region_name = 'us-west-2',
  s3.bucket_name = 'my-bucket',
  format = 'parquet',
  type = 'append-only'
);
```

#### Materialize

**Persist Storage System:**
- ✅ S3 as persistent backend
- ✅ Elastic storage (AWS S3)
- ✅ Compute/storage separation
- ✅ pTVCs (persistent time-varying collections) in S3
- ✅ Distributed transactional database for consensus

**Architecture:**
```
Compute Clusters → Persist → S3
                      ↓
          Storage Clusters → S3
                      ↓
           environmentd → S3
```

All data shared between clusters flows through **Persist + S3**.

#### Arroyo

**FileSystem Connector:**
- ✅ Local filesystem
- ✅ S3 (+ S3-compatible like MinIO)
- ✅ GCS

**Supported Formats:**
- ✅ JSON
- ✅ Parquet
- ✅ Various compression codecs

**Use Case - Bootstrapping:**
```sql
-- Historical data from S3
CREATE TABLE historical_data WITH (
  connector = 'filesystem',
  type = 'source',
  path = 's3://my-bucket/history/',
  format = 'parquet'
);

-- Real-time data from Kafka
CREATE TABLE realtime_data WITH (
  connector = 'kafka',
  ...
);

-- Union both!
SELECT * FROM historical_data
UNION ALL
SELECT * FROM realtime_data;
```

### Blob/File Storage Recommendations for Boon

**For BYTES { } streaming (large files/video):**

1. **Server Domain:**
   - **RisingWave** - Best S3 integration, S3 as primary storage
   - **Arroyo** - Good for hybrid S3 + Kafka bootstrapping
   - **Feldera** - When working with Delta Lake/Iceberg data lakes

2. **Software Domain (Browser):**
   - Use **NATS Object Store** (as originally planned in TABLE_BYTES_RESEARCH.md)
   - Differential Dataflow for metadata/pointers
   - Actual blobs in NATS Object Store

3. **Hybrid Architecture:**
```boon
// Server: Large video file
video: BYTES {}
    |> Bytes/stream_from_s3(
        bucket: TEXT { my-bucket }
        key: TEXT { videos/demo.mp4 }
    )
    |> Bytes/backend(
        storage: Backend/risingwave_s3(
            region: TEXT { us-west-2 }
        )
    )

// Browser: Stream chunks via NATS
video_chunks: video
    |> Bytes/stream_chunks(size: megabytes(1))
    |> Bytes/sync_via_nats()
```

**Key Insight:**

For Boon's **triple store + incremental computation** model:
- **Metadata/triples** → DBSP/Differential Dataflow
- **Large blobs (BYTES)** → S3 (server) or NATS Object Store (distributed)
- **Pointers** in triple store, **data** in object store

---

## Next Steps: Integration Research

1. **DBSP WASM Support:**
   - Can DBSP compile to WASM for browser?
   - If not, can Differential Dataflow?

2. **Triple Store Mapping:**
   - How to map [entity, attribute, value] to DBSP/Differential operators?
   - Performance of EAV vs columnar?

3. **Query Language Design:**
   - Compile Boon TABLE/RELATION/GRAPH to SQL or Datalog?
   - Direct to DBSP operators?

4. **Prototype:**
   - Small proof-of-concept with Differential Dataflow
   - Benchmark: triple store incremental queries
   - Test WASM compilation

---

## References

**Papers:**
- "Naiad: A Timely Dataflow System" (SOSP 2013) - Timely Dataflow foundation
- "DBSP: Automatic Incremental View Maintenance" (VLDB 2023) - **Best Paper**
- "Hydroflow: A Model and Runtime for Distributed Systems" (OOPSLA 2023)
- "Flo: Semantic Foundation for Progressive Stream Processing" (POPL 2025)

**Repositories:**
- Timely Dataflow: https://github.com/TimelyDataflow/timely-dataflow
- Differential Dataflow: https://github.com/TimelyDataflow/differential-dataflow
- DBSP (Feldera): https://github.com/feldera/feldera
- Differential Datalog: https://github.com/vmware-archive/differential-datalog
- Materialize: https://github.com/MaterializeInc/materialize
- RisingWave: https://github.com/risingwavelabs/risingwave
- Arroyo: https://github.com/ArroyoSystems/arroyo
- Hydroflow: https://github.com/hydro-project/hydro
- Datafrog: https://github.com/rust-lang/datafrog

**Key People:**
- **Frank McSherry:** Created Timely Dataflow, Differential Dataflow, Materialize, Datafrog
- **Joe Hellerstein:** UC Berkeley, leads Hydroflow/Hydro Project
- **Feldera Team:** DBSP creators (VLDB 2023 Best Paper)

---

**Conclusion:** The Rust streaming/incremental computation ecosystem is **production-ready** and **actively evolving**. DBSP/Feldera is the most mature option for database workloads. Differential Dataflow provides the proven foundation. Hydroflow represents cutting-edge research in formal semantics.

For Boon, **DBSP (Feldera) + Differential Dataflow** hybrid approach provides the best of both worlds: SQL compilation for server, Rust API for software/hardware.
