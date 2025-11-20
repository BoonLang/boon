# Unison Language Analysis: Lessons for Boon

Research into Unison programming language's revolutionary approach to code storage, distributed computing, and effect systems.

**Website:** https://www.unison-lang.org/
**Repository:** https://github.com/unisonweb/unison
**License:** MIT
**Status:** Production (General Availability 2024)
**Language:** Haskell (compiler), Unison (self-hosted)

---

## Revolutionary Core Concepts

### 1. Content-Addressed Code

**The Big Idea:** Each Unison definition receives a unique identifier based on a **hash of its syntax tree**, not its name.

**How It Works:**
- Function `foo` is identified by `#abc123...` (512-bit SHA3 hash)
- Hash includes all dependencies (recursive hashing)
- Names are **metadata**, not identity
- Definitions are **immutable** once hashed

**Example:**
```unison
-- First version
square : Nat -> Nat
square x = x * x
-- Hash: #abc123

-- Rename (hash unchanged!)
mult : Nat -> Nat
mult x = x * x
-- Still hash: #abc123

-- Change implementation (new hash!)
square : Nat -> Nat
square x = x Nat.* x
-- Hash: #def456
```

**Benefits:**
- ✅ **No dependency conflicts** (two versions coexist peacefully)
- ✅ **Perfect caching** (parse/typecheck results cached forever)
- ✅ **Instant renaming** (name is just metadata)
- ✅ **Distributed deployment** (send missing definitions by hash)

**Collision probability:** First collision expected after ~100 quadrillion years of generating 1 million definitions/second.

---

### 2. Immutable Codebase Database

**Storage Model:** Unison codebase is an **append-only database**, not text files.

**Structure:**
```
Codebase (SQLite-based):
  Definitions:
    #abc123 → AST (square function)
    #def456 → AST (updated square)
    #ghi789 → AST (User type)

  Names (metadata):
    square → #def456  (currently points to new version)
    User → #ghi789

  Dependencies:
    #def456 depends on [Nat.*, ...]

  Type Index:
    Nat -> Nat → [#abc123, #def456, ...]
```

**Key Properties:**
- **Append-only:** Definitions never change, only add new ones
- **Typed:** Database knows the type of everything
- **Perfect compilation cache:** Typechecking cached by hash
- **Dependency tracking:** Complete graph of dependencies
- **Type-based search:** Find all functions of type `Nat -> Nat`

**For Boon:** This is similar to our **triple store + content addressing** idea!

---

### 3. Names as Pointers, Not Identity

**Critical Insight:** Names can change what they point to, but the content at each hash is **forever unchanging**.

**Example:**
```
Day 1:
  Email → #hash_v1  (simple email validation)

Day 30:
  Email → #hash_v2  (updated with better validation)

Day 60:
  OldEmail → #hash_v1  (preserve old version)
  Email → #hash_v2     (current version)
```

**Result:** Both versions coexist! Code using `#hash_v1` continues working, code using `#hash_v2` gets new behavior.

**For Boon:** This solves schema evolution! Like our triple store approach:
```boon
[user_1, email, TEXT { alice@example.com }]  // Uses Email v1
[user_2, email_v2, TEXT { bob@example.com }]  // Uses Email v2
```

---

### 4. Typed Durable Storage

**Problem in traditional systems:**
```
// Day 1: Save user
serialize(User { name: "Alice", age: 30 })

// Day 30: Update User type
type User = { name: Text, age: Nat, email: Text }

// Deserialize old data → BREAKS!
deserialize(old_blob) // Missing 'email' field!
```

**Unison solution:**
```unison
-- Serialization stores HASH, not structure
Value.serialize user_1  -- Stores: #hash_User_v1 + data

-- Deserialization always works
Value.deserialize blob  -- Gets typed reference to #hash_User_v1
-- Type system KNOWS it's the old version!
-- No breaking changes!
```

**Benefits:**
- ✅ Values serialized with content hash
- ✅ Deserialization always gets correct type
- ✅ No breaking changes from type evolution
- ✅ Storage layer is **typed** (not like SQL's separate types)

**For Boon:** Perfect for our **persistence** model!
```boon
// Serialize with triple + hash
[user_1, data, HASH { #type_User_v1 }, BYTES { ... }]

// Deserialize always gets right type
user: users |> Table/get(user_1)  // Type system knows it's v1
```

---

## Abilities: Algebraic Effects System

### What Are Abilities?

**Abilities** = Unison's implementation of algebraic effects (based on Frank language)

**Core Idea:** Effects are **visible in type signatures**, making programs more explicit and composable.

**Syntax:**
```unison
-- Pure function (no abilities)
square : Nat -> Nat
square x = x * x

-- Function with abilities
printSquare : Nat ->{IO, Exception} ()
printSquare x =
  result = square x
  printLine (Nat.toText result)
```

**Type signature format:** `Input ->{Ability1, Ability2} Output`

**Empty abilities = pure:** `A ->{} B` (no side effects)

---

### User-Defined Abilities

**Define custom abilities:**
```unison
structural ability Store a where
  Store.put : a ->{Store a} ()
  Store.get : {Store a} a

structural ability Logger where
  Logger.log : Text ->{Logger} ()
```

**Abilities are:**
- Interfaces specifying operations
- Visible in type signatures
- Composable (multiple abilities in one function)

---

### Ability Handlers

**Handlers provide implementations:**

```unison
storeHandler : v -> Request (Store v) a -> a
storeHandler storedValue = cases
  {Store.get -> k} ->
    -- Return stored value, continue with handler
    handle k storedValue with storeHandler storedValue

  {Store.put newValue -> k} ->
    -- Update stored value, continue with handler
    handle k () with storeHandler newValue

  {result} ->
    -- Pure value, return it
    result

-- Usage
result = handle
  value1 = Store.get
  Store.put (value1 + 1)
  value2 = Store.get
  value2
with storeHandler 0

-- Result: 1
```

**Key Features:**
- Handlers pattern match on ability operations
- `k` is the **continuation** (rest of the computation)
- Handlers can call continuation 0, 1, or multiple times
- State threading via recursive handler calls

---

### Abilities vs Monads

**Monads (Haskell):**
```haskell
do
  value <- State.get
  State.put (value + 1)
  value' <- State.get
  return value'
```

**Abilities (Unison):**
```unison
value = Store.get
Store.put (value + 1)
value' = Store.get
value'
```

**Advantages:**
- ✅ Normal syntax (no special `do` notation)
- ✅ Multiple effects compose naturally
- ✅ Handlers decouple interface from implementation
- ✅ More flexible than monads (can short-circuit, backtrack, etc.)

---

### Built-In Abilities

**Common abilities in Unison:**

- `{IO}` - Input/output operations
- `{Exception}` - Error handling (throw/catch)
- `{Remote}` - Distributed computation
- `{Stream s}` - Stream processing

**Example:**
```unison
processFile : Text ->{IO, Exception} [Text]
processFile path =
  contents = readFile path
  lines = Text.split "\n" contents
  lines
```

---

## Distributed Computing

### The Remote Ability

**Revolutionary:** Describe distributed systems in the **same language** as the programs themselves.

**Core API:**
```unison
-- Fork computation at location
forkAt : Location -> '{Remote} a ->{Remote} Future a

-- Wait for result
await : Future a ->{Remote} a

-- Current location
here : {Remote} Location
```

**Example - Distributed Map:**
```unison
distributedMap : (a ->{Remote} b) -> [a] ->{Remote} [b]
distributedMap f items =
  locations = Remote.locations  -- Available nodes
  futures = List.map2 items locations (loc, item ->
    forkAt loc '(f item)
  )
  List.map futures await
```

**How it works:**
1. `forkAt location computation` sends computation to remote node
2. Missing dependencies deployed automatically (by hash!)
3. Computation runs remotely
4. `await` gets result back

**No containers, no orchestration, no YAML!** Just function calls.

---

### Content-Addressed Deployment

**Traditional deployment:**
```
1. Build container image
2. Push to registry
3. Update Kubernetes YAML
4. kubectl apply
5. Hope dependencies match
```

**Unison deployment:**
```unison
-- Just call a function!
deploy.http "/" myHandler

-- Code and dependencies deployed automatically by hash
-- No build, no containers, no config
```

**Why it works:**
- Code identified by content hash
- Runtime fetches missing definitions automatically
- Statically typed (no runtime errors from mismatched versions)
- No dependency conflicts (hashes ensure correct versions)

---

### Distributed Datasets

**Pattern:** Wrap fields in `Remote.Value` to make data structures distributed.

**Local tree:**
```unison
Tree a =
  | One a
  | Two (Tree a) (Tree a)
  | Empty
```

**Distributed tree:**
```unison
DistTree a =
  | One (Remote.Value a)
  | Two (Remote.Value (DistTree a)) (Remote.Value (DistTree a))
  | Empty
```

**Operations:**
```unison
-- Lazy map (computation moves to data!)
Tree.map : (a -> b) -> DistTree a -> DistTree b
Tree.map f = cases
  One v -> One (Value.map f v)
  Two left right ->
    Two (Value.map (Tree.map f) left)
        (Value.map (Tree.map f) right)
  Empty -> Empty

-- Fusion: multiple maps compose without intermediate structures
tree
  |> Tree.map (x -> x + 1)
  |> Tree.map (x -> x * 2)
  |> Tree.map (x -> x - 5)
-- All three operations fuse into one pass!
```

**Principle:** Move computation to data, not data to computation.

---

### Unison Cloud

**Deployment model:**
```unison
-- HTTP service
myApi : HttpRequest ->{IO, Exception} HttpResponse
myApi req = ...

-- Deploy with single function call!
deploy.http "/api" myApi

-- Runs on elastic pool of Unison nodes
-- Auto-scaling, fault tolerance, all automatic
```

**Architecture:**
- Content-addressed code (no containers)
- Elastic node pool (pay per use)
- Automatic dependency deployment
- Typed service communication (by hash)

**BYOC (Bring Your Own Cloud):** Run on your infrastructure, only needs S3-compatible storage.

---

## Key Insights for Boon

### 1. Content-Addressed Everything

**Unison Approach:**
- Functions identified by hash
- Immutable definitions
- Names are metadata

**For Boon:**
```boon
// Content-addressed types
User : TYPE { #hash_type_v1
    name: TEXT
    email: TEXT
}

// Content-addressed functions
validate_email : FUNCTION { #hash_fn_v1
    input: TEXT
    output: Bool
    impl: { ... }
}

// Content-addressed data
[user_1, type, HASH { #hash_type_v1 }]
[user_1, name, TEXT { Alice }]
[user_1, email, TEXT { alice@... }]
```

**Benefits for Boon:**
- ✅ Schema evolution without migrations (like our triple store!)
- ✅ Multiple versions coexist (old code keeps working)
- ✅ Distributed deployment (send missing code by hash)
- ✅ Perfect caching (compilation results cached forever)

---

### 2. Abilities for Effect Management

**Unison Approach:**
- Effects visible in type signatures
- User-defined abilities
- Handlers decouple interface/implementation

**For Boon:**
```boon
// Define ability
ABILITY Store {
    get: {Store} Value
    put: Value -> {Store} ()
}

// Use ability
process: {Store, IO} Result
process = {
    current = Store.get
    Store.put(current + 1)
    IO.print(current)
}

// Provide handler
HANDLER LocalStore(initial) {
    Store.get -> {
        state, continue(state)
    }
    Store.put(new_value) -> {
        new_value, continue(())
    }
}

// Run with handler
result = process
    |> Handle/with(LocalStore(0))
```

**Benefits for Boon:**
- ✅ Explicit effects (know what functions do)
- ✅ Composable (multiple abilities)
- ✅ Testable (swap handlers for testing)
- ✅ Backend-agnostic (same code, different handlers)

**Handlers for domains:**
```boon
// Hardware domain handler
HANDLER HardwareStore {
    Store.get -> read_from_bram(address)
    Store.put(val) -> write_to_bram(address, val)
}

// Software domain handler
HANDLER BrowserStore {
    Store.get -> LocalStorage.get(key)
    Store.put(val) -> LocalStorage.set(key, val)
}

// Server domain handler
HANDLER PostgresStore {
    Store.get -> Postgres.query(TEXT { SELECT ... })
    Store.put(val) -> Postgres.execute(TEXT { INSERT ... })
}

// Same Boon code, different handlers per domain!
```

---

### 3. Typed Durable Storage

**Unison Approach:**
- Serialize with content hash
- Deserialize always gets correct type
- No breaking changes from schema evolution

**For Boon:**
```boon
// Serialize user (with type hash)
user_blob = user
    |> Value/serialize(type_hash: #User_v1)
    |> Bytes/to_s3(bucket, key)

// Later: deserialize (type system knows version!)
loaded_user = Bytes/from_s3(bucket, key)
    |> Value/deserialize()
// Type: User_v1 (not User_v2, even if User evolved!)
```

**Benefits:**
- ✅ No schema migrations
- ✅ Old data always deserializes correctly
- ✅ Type system tracks versions
- ✅ Storage layer is typed (like triple store!)

---

### 4. Distributed Computing Simplified

**Unison Approach:**
- `forkAt` sends computation to location
- Content-addressed deployment
- No containers/orchestration

**For Boon:**
```boon
// Distributed query (across cluster)
result = TABLE_DATA
    |> Table/distribute_across(cluster_nodes)
    |> Table/map(heavy_computation)
    |> Table/reduce(combine)

// Under the hood:
// 1. Boon compiler generates content hash for heavy_computation
// 2. Runtime sends hash to nodes
// 3. Nodes fetch missing code if needed
// 4. Computation runs in parallel
// 5. Results combined
```

**Benefits:**
- ✅ No explicit deployment (automatic)
- ✅ No version conflicts (content-addressed)
- ✅ Type-safe (statically checked)

---

### 5. Immutable Codebase = Triple Store Analogy

**Unison Codebase:**
```
#hash_1 → Definition (immutable)
#hash_2 → Definition (immutable)
name1 → #hash_1 (mutable pointer)
name2 → #hash_2 (mutable pointer)
```

**Boon Triple Store:**
```
[entity_1, attribute_1, value_1]  (immutable triple)
[entity_2, attribute_2, value_2]  (immutable triple)

// Add new triple (don't modify existing)
[entity_1, attribute_1, value_2]  (new version)
```

**Parallel Concepts:**
- Both are **append-only**
- Both have **immutable facts**
- Both support **multiple versions** coexisting
- Both enable **schema evolution** without breaking changes

---

## Comparison: Unison vs Boon Design

| Feature | Unison | Boon (Planned) |
|---------|--------|----------------|
| **Code Storage** | Content-addressed AST database | Triple store ([entity, attribute, value]) |
| **Immutability** | Definitions immutable | Triples immutable |
| **Schema Evolution** | Multiple hashes coexist | Multiple triples coexist |
| **Effect System** | Abilities + Handlers | Abilities? (inspired by Unison) |
| **Distributed** | Remote ability + content deployment | Incremental dataflow + content addressing |
| **Persistence** | Typed durable storage (hash-based) | Triple store + S3 (RisingWave) |
| **Reactivity** | Not primary focus | Core feature (incremental computation) |
| **Type System** | Hindley-Milner + abilities | ? (to be designed) |
| **Caching** | Perfect (hash-based) | Perfect (incremental dataflow) |

---

## Concrete Recommendations for Boon

### 1. Adopt Content-Addressed Types

**Recommendation:** Hash type definitions and store in triple store.

```boon
// Define type with content hash
User : TYPE {
    name: TEXT
    email: TEXT
}
// Compiler generates: #hash_User_v1

// Store in triple store
[#hash_User_v1, field, name, TEXT]
[#hash_User_v1, field, email, TEXT]

// Entity references type by hash
[user_1, type, #hash_User_v1]
[user_1, name, TEXT { Alice }]

// Later: evolve type (new hash!)
User : TYPE {
    name: TEXT
    email: TEXT
    phone: TEXT  // New field
}
// Compiler generates: #hash_User_v2

// Old data still valid!
[user_1, type, #hash_User_v1]  // Old version
[user_2, type, #hash_User_v2]  // New version
```

**Benefits:**
- No migrations
- Multiple versions coexist
- Type system knows which version

---

### 2. Implement Abilities for Effects

**Recommendation:** Add ability system to Boon for managing effects across domains.

```boon
// Define abilities
ABILITY IO {
    read: Path -> {IO} BYTES
    write: Path, BYTES -> {IO} ()
}

ABILITY Store {
    get: Key -> {Store} Value
    put: Key, Value -> {Store} ()
}

// Use in functions
process_file: Path -> {IO, Store} Result
process_file = { path ->
    data = IO.read(path)
    Store.put(TEXT { cache }, data)
    parse(data)
}

// Handlers per domain
HANDLER BrowserIO {
    IO.read(path) -> fetch(path)
    IO.write(path, data) -> LocalStorage.set(path, data)
}

HANDLER ServerIO {
    IO.read(path) -> S3.get(bucket, path)
    IO.write(path, data) -> S3.put(bucket, path, data)
}
```

**Benefits:**
- Explicit effects (know what code does)
- Backend-agnostic (swap handlers per domain)
- Testable (mock handlers)

---

### 3. Content-Addressed Code Deployment

**Recommendation:** Use content hashing for distributing Boon programs across cluster.

```boon
// Function with hash
heavy_computation: Data -> Result
heavy_computation = { data -> ... }
// Hash: #fn_heavy_v1

// Distributed execution
results = dataset
    |> Table/distribute(cluster_nodes)
    |> Table/map(heavy_computation)  // Sends #fn_heavy_v1 to nodes
    |> Table/collect()

// Runtime:
// 1. Serialize #fn_heavy_v1
// 2. Send to nodes
// 3. Nodes check if they have #fn_heavy_v1
// 4. If missing, fetch from coordinator
// 5. Execute locally
```

**Benefits:**
- No manual deployment
- No version conflicts
- Cache by hash (execute once, cache forever)

---

### 4. Typed Persistence with Hashes

**Recommendation:** Store type hashes with data in triple store.

```boon
// Serialize with type hash
user_data = User { name: TEXT { Alice }, email: TEXT { alice@... } }
    |> Serialize/with_hash()
// Stores: [type_hash: #User_v1, data: {...}]

// Persist to S3
user_blob = user_data
    |> Bytes/to_triple_store()
// Triple: [user_1, data, HASH { #User_v1 }, BYTES { ... }]

// Deserialize (type system knows version!)
loaded = triple_store
    |> Triple/get([user_1, data, ?, ?])
    |> Bytes/deserialize()
// Type: User_v1 (even if current User is v2!)
```

---

### 5. Immutable Triple Store (Like Unison Codebase)

**Recommendation:** Embrace append-only triple store, never modify existing triples.

```boon
// Day 1: Create user
[user_1, name, TEXT { Alice }]
[user_1, email, TEXT { alice@example.com }]

// Day 30: Update email (DON'T modify existing triple!)
[user_1, email_v2, TEXT { alice@newdomain.com }]
[user_1, email_deprecated, TEXT { alice@example.com }]  // Keep old

// Query latest email
user_email = triples
    |> Triple/query([user_1, email_v2, ?email])

// Or: Query historical email
old_email = triples
    |> Triple/query([user_1, email_deprecated, ?email])
```

**Benefits:**
- Time-travel queries (see old versions)
- No breaking changes
- Audit trail (all history preserved)

---

## Architecture Synthesis: Unison + Boon

**Combining best of both:**

```
┌─────────────────────────────────────────────┐
│     Boon Language (TABLE, RELATION, etc.)   │
│   + Content-Addressed Types (Unison-inspired)│
│   + Abilities for Effects (Unison-inspired) │
└──────────────────┬──────────────────────────┘
                   │
                   ↓
┌─────────────────────────────────────────────┐
│   Triple Store (Immutable, Append-Only)     │
│   - Store type hashes (like Unison)         │
│   - Store code hashes (for distribution)    │
│   - Store data with type references         │
└──────────────────┬──────────────────────────┘
                   │
                   ↓
┌─────────────────────────────────────────────┐
│   Incremental Computation (DBSP/Differential)│
│   - Reactive (100,000x speedup)             │
│   - Caches by hash (like Unison)            │
└──────────────────┬──────────────────────────┘
                   │
                   ↓
┌─────────────────────────────────────────────┐
│        Backend Storage (Domain-Specific)    │
│   Hardware: BRAM (content-addressed)        │
│   Software: IndexedDB (hash keys)           │
│   Server: RisingWave S3 (hash-based blobs)  │
└─────────────────────────────────────────────┘
```

**Key Synergies:**

1. **Unison's Content Addressing** + **Boon's Triple Store** = Schema-less evolution
2. **Unison's Abilities** + **Boon's Cross-Domain** = Backend-agnostic effects
3. **Unison's Immutable Codebase** + **Boon's Append-Only Triples** = Time-travel queries
4. **Unison's Typed Storage** + **Boon's Persistence** = No migrations
5. **Unison's Distributed Computing** + **Boon's Incremental Dataflow** = Reactive distributed systems

---

## What Boon Should NOT Take from Unison

**1. Text-Based Codebase Manager:**
- Unison uses UCM (Unison Codebase Manager) CLI
- Boon should integrate with existing editors/IDEs

**2. Complete Departure from Files:**
- Unison stores everything in database
- Boon should support both (files for simple cases, DB for complex)

**3. Haskell-Style Syntax:**
- Unison uses Haskell-like syntax
- Boon has its own pipe-based syntax (keep it!)

**4. Cloud-First Philosophy:**
- Unison optimized for Unison Cloud
- Boon targets hardware/software/server equally

---

## Implementation Priorities for Boon

**Phase 1: Content-Addressed Types**
1. Hash type definitions
2. Store type hashes in triple store
3. Reference types by hash (not name)

**Phase 2: Abilities System**
1. Define ability syntax in Boon
2. Implement handler mechanism
3. Standard abilities: IO, Store, Remote

**Phase 3: Content-Addressed Code**
1. Hash function definitions
2. Store code in triple store
3. Enable distribution by hash

**Phase 4: Typed Persistence**
1. Serialize with type hash
2. Deserialize with version tracking
3. Support schema evolution

**Phase 5: Distributed Deployment**
1. Content-addressed deployment
2. Automatic dependency fetching
3. No-container distributed execution

---

## References

**Core Docs:**
- Big Idea: https://www.unison-lang.org/docs/the-big-idea/
- Abilities: https://www.unison-lang.org/docs/language-reference/abilities-and-ability-handlers/
- Distributed Datasets: https://www.unison-lang.org/articles/distributed-datasets/core-idea/

**Papers:**
- Frank language (algebraic effects): Lindley, McBride, McLaughlin

**Production:**
- Unison Cloud: https://www.unison.cloud/
- GitHub: https://github.com/unisonweb/unison (MIT license)

**Key People:**
- Paul Chiusano: Creator
- Rúnar Bjarnason: Core contributor

---

**Conclusion:** Unison's content-addressed code + immutable codebase + abilities system provides revolutionary approaches that **align perfectly** with Boon's triple store + incremental computation + cross-domain philosophy. Adopting content-addressed types and abilities would make Boon even more powerful for reactive, distributed, schema-less applications.
