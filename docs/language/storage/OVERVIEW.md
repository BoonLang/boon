# Storage Research Overview

This directory contains comprehensive research into Boon's storage and data layer design.

## Research Documents

### 1. [TABLE_BYTES_RESEARCH.md](TABLE_BYTES_RESEARCH.md)

**Focus:** Key-value storage with backend synchronization

**Key Concepts:**
- TABLE construct for key-value storage (dynamic and fixed-size)
- Hardware implementations (CAM, Hash+BRAM, BTree)
- ElectricSQL integration for cell-level reactivity
- BYTES streaming with NATS Object Store
- Backend configuration in Boon syntax

**Example:**
```boon
users: TABLE { UserId, User }
    |> Table/backend(
        postgres: Backend/electric_sql(
            url: Env/var(TEXT { DATABASE_URL })
            sync_granularity: Cell
        )
    )
```

### 2. [RELATION_GRAPH_ANALYSIS.md](RELATION_GRAPH_ANALYSIS.md)

**Focus:** Multi-model database approach

**Key Concepts:**
- Cell language's relation model (omnidirectional queries)
- SurrealDB's multi-model design (document + graph + relational)
- Three data models: TABLE, RELATION, GRAPH
- Typed IDs and relationship-first thinking

**Example:**
```boon
// RELATION: omnidirectional queries
usernames: RELATION { UserId, Text }
name: usernames |> Relation/get(user_id)           // Forward
id: usernames |> Relation/get_reverse(TEXT { alice })  // Reverse

// GRAPH: traversal and relationships
wrote: GRAPH { from: users, to: posts }
alice_posts: wrote |> Graph/from(user: alice_id) |> Graph/to_nodes()
```

### 3. [DATALOG_INCREMENTAL_RESEARCH.md](DATALOG_INCREMENTAL_RESEARCH.md)

**Focus:** Triple store + Datalog + Incremental computation

**Key Concepts:**
- InstantDB's triple store architecture ([entity, attribute, value])
- Differential Dataflow (100,000x speedup on updates)
- DBSP/Feldera (SQL/Datalog to incremental operators)
- ElectricSQL internals (shape-based sync)
- Permission rules and derived data (forward chaining)

**Example:**
```boon
// Triple store (schema-less)
[alice, name, TEXT { Alice }]
[alice, email, TEXT { alice@example.com }]
[post_1, author, alice]

// Reactive query (Datalog)
alice_posts: QUERY {
    ?user WHERE [?user, name, TEXT { Alice }]
    ?post WHERE [?post, author, ?user]
    SELECT post
}

// Incremental update: 72 seconds → 0.5 milliseconds!
```

### 4. [RUST_STREAMING_ECOSYSTEM.md](RUST_STREAMING_ECOSYSTEM.md)

**Focus:** Rust libraries for streaming, dataflow, and incremental computation

**Key Systems Covered:**

**Foundations:**
- **Timely Dataflow** - Low-latency cyclic dataflow (foundation for everything)
- **Differential Dataflow** - Incremental computation (100,000x speedup)

**High-Level Languages:**
- **DBSP (Feldera)** - SQL/Datalog to incremental operators (VLDB 2023 Best Paper)
- **Differential Datalog (DDlog)** - Datalog language compiling to Differential Dataflow

**Streaming Databases:**
- **Materialize** - Streaming SQL using Differential Dataflow (Postgres-compatible)
- **RisingWave** - Cloud-native streaming DB (custom Rust implementation)

**Stream Processing:**
- **Arroyo** - Serverless stream processing (Flink alternative, 10x faster)
- **Hydroflow** - UC Berkeley's formal dataflow runtime (POPL 2025)

**Specialized:**
- **Datafrog** - Simple Datalog for Rust's Polonius borrow checker

**Key Insight:**

The Rust streaming ecosystem is **production-ready** with multiple mature options. **DBSP/Feldera** is the best choice for database workloads (compiles SQL/Datalog), while **Differential Dataflow** provides the proven foundation.

**Recommendation for Boon:**
```
Hardware Domain: Datafrog (simple, fast)
Software Domain: Differential Dataflow (incremental, WASM)
Server Domain: DBSP/Feldera (SQL, Postgres backend)
```

### 5. [UNISON_LANGUAGE_ANALYSIS.md](UNISON_LANGUAGE_ANALYSIS.md)

**Focus:** Content-addressed code, abilities (algebraic effects), distributed computing

### 6. [IMPLEMENTATION_GUIDE.md](IMPLEMENTATION_GUIDE.md)

**Focus:** Practical decision-making, open questions, pitfalls, and domain-specific recommendations

**What's Inside:**

**Decision Matrices:**
- When to use DBSP vs Differential Dataflow vs RisingWave
- When to use which storage backend (S3, NATS, IndexedDB)
- Decision trees for choosing tech stack per domain

**Open Questions:**
- Can DBSP compile to WASM?
- EAV vs columnar performance?
- Content-addressing implementation details
- Abilities system exact syntax
- Cell-level reactivity with triples

**Potential Pitfalls:**
- BSL license trap (Materialize restrictions)
- DBSP distributed not ready yet
- EAV table performance challenges
- Content-addressing overhead
- Distributed debugging complexity
- Schema evolution edge cases
- WASM performance limitations

**Domain Comparison:**
- **Hardware:** Datafrog + BRAM/CAM, fixed-size, CRC32 hashing
- **Software:** Differential Dataflow + IndexedDB + NATS, WASM, offline-first
- **Server:** RisingWave + Postgres + S3, distributed cluster, elastic scaling

**Implementation Phases:**
1. Prototype triple store (2-4 weeks)
2. Server backend with RisingWave (3-6 weeks)
3. Content-addressed types (2-3 weeks)
4. Abilities system (4-6 weeks)
5. Distributed deployment (6-8 weeks)
6. Hardware synthesis (8-12 weeks, optional)

---

## The Unification: All Collapse to Triple Store

**Critical Insight:** All three approaches (TABLE, RELATION, GRAPH) collapse into the same underlying foundation: **Triple Store + Datalog + Incremental Computation**.

### How They Unify

**TABLE is syntactic sugar for triples:**
```boon
users: TABLE { UserId, User }
// Internally: [user_1, name, TEXT { Alice }], [user_1, email, TEXT { ... }]
```

**RELATION is native triples:**
```boon
usernames: RELATION { UserId, Text }
// Stored as: [user_1, username, TEXT { alice }]
// Omnidirectional queries = triple pattern matching!
```

**GRAPH is triples with edge semantics:**
```boon
wrote: GRAPH { from: users, to: posts }
// Stored as: [alice, wrote, post_1]
// Traversal = triple pattern matching!
```

### The Unified Architecture

```
Boon Syntax (User Writes)
  TABLE, RELATION, GRAPH, QUERY, RULE
            ↓
  Datalog Compiler (Boon → Datalog)
            ↓
  DBSP Incremental Engine (100,000x speedup)
            ↓
  Triple Store Backend
    Hardware: CAM/BRAM
    Software: In-memory (HashMap/BTree)
    Server: Postgres EAV + NATS
            ↓
  ElectricSQL Sync Layer (cell-level reactivity)
```

### Why This Is Revolutionary

**Benefits:**
- ✅ **Schema-less:** No migrations! Add attributes dynamically
- ✅ **Graph-native:** Relations are triples
- ✅ **100,000x faster:** Incremental computation on updates
- ✅ **Fine-grained reactivity:** Cell-level (subscribe to specific [entity, attribute])
- ✅ **Declarative queries:** Datalog for composable queries
- ✅ **Secure by default:** Permission rules in query engine
- ✅ **Unified model:** TABLE + RELATION + GRAPH all compile to triples
- ✅ **Cross-domain:** Hardware/Software/Server use same foundation

### Example: All Three Models Together

```boon
// 1. TABLE for key-value
users: TABLE { UserId, User }

// 2. RELATION for omnidirectional queries
usernames: RELATION { UserId, Text }

// 3. GRAPH for relationships
wrote: GRAPH { from: users, to: posts }

// 4. QUERY (Datalog) over ANY of them!
alice_posts: QUERY {
    ?user WHERE [?user, username, TEXT { alice }]  // RELATION
    ?post WHERE [?user, wrote, ?post]              // GRAPH
    ?title WHERE [?post, title, ?title]            // TABLE attribute
    SELECT [post, title]
}

// All compile to triple patterns!
// All benefit from incremental computation!
// All synchronized via ElectricSQL!
```

### Schema Evolution Without Migrations

```boon
// Day 1: Users have name and email
[alice, name, TEXT { Alice }]
[alice, email, TEXT { alice@example.com }]

// Day 30: Add phone (NO MIGRATION!)
[alice, phone, TEXT { 555-1234 }]

// Day 60: Add profile settings (NO MIGRATION!)
[alice, theme, TEXT { dark }]
[alice, notifications, True]

// Day 90: Add social connections (NO MIGRATION!)
[alice, follows, bob]

// No ALTER TABLE! Just insert triples!
```

---

## Recommended Reading Order

1. **Start with TABLE_BYTES_RESEARCH.md** - Understand basic TABLE construct and ElectricSQL sync
2. **Then RELATION_GRAPH_ANALYSIS.md** - See how RELATION and GRAPH extend the model
3. **Then DATALOG_INCREMENTAL_RESEARCH.md** - Discover how everything unifies into triple store + Datalog
4. **Then RUST_STREAMING_ECOSYSTEM.md** - Survey implementation options (Timely, Differential, DBSP, Hydroflow, etc.)
5. **Then UNISON_LANGUAGE_ANALYSIS.md** - Learn about content-addressed code and abilities for inspiration
6. **Finally IMPLEMENTATION_GUIDE.md** - Practical decision-making, pitfalls, and implementation phases

The synthesis section in DATALOG_INCREMENTAL_RESEARCH.md (at the end) shows the complete unification. RUST_STREAMING_ECOSYSTEM.md provides concrete implementation choices. UNISON_LANGUAGE_ANALYSIS.md offers revolutionary concepts that align with Boon's philosophy. IMPLEMENTATION_GUIDE.md gives practical guidance for building it.

---

## Next Steps

**Foundation:** Triple Store + Datalog + Incremental Computation

**User-Facing Syntax:** TABLE, RELATION, GRAPH, QUERY, RULE

**Implementation Path:**
1. Prototype triple store in playground (in-memory)
2. Integrate DBSP/Differential Dataflow for incremental computation
3. Design Datalog compiler (Boon → Datalog → DBSP)
4. Test ElectricSQL integration (Postgres EAV + shape-based sync)
5. Implement permission rules (CEL-like syntax)

**Key Technologies:**

**Core Foundations:**
- **Timely Dataflow** - Low-latency cyclic dataflow (foundation)
- **Differential Dataflow** - Incremental computation (100,000x speedup)
- **DBSP (Feldera)** - SQL/Datalog compiler to incremental operators (VLDB 2023 Best Paper)

**Alternatives Considered:**
- **Hydroflow** - UC Berkeley's formal dataflow runtime (POPL 2025)
- **Arroyo** - Serverless stream processing (Flink alternative)
- **Materialize** - Production streaming SQL database (uses Differential Dataflow)
- **RisingWave** - Cloud-native streaming DB (custom Rust)
- **Datafrog** - Simplified Datalog (Rust compiler's Polonius)

**Sync & Storage:**
- **ElectricSQL** - Cell-level reactivity (Postgres logical replication)
- **Postgres** - Server-side storage (EAV table for triples)
- **NATS** - Distributed sync and streaming

---

**The future of Boon's data layer is Incremental Datalog with Triple Store foundation!**
