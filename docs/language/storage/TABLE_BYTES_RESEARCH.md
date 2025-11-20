# TABLE & BYTES: Reactive Storage & Streaming

**Date**: 2025-01-20
**Status**: Research & Design Proposal
**Scope**: TABLE for key-value storage, BYTES for streaming files

---

## Executive Summary

**TABLE** provides reactive key-value storage that works across all Boon domains:
- **Hardware**: CAM, Hash+BRAM, or BTree synthesis
- **Software**: Dynamic hash tables with unlimited size
- **Server**: Transparent sync with backends (ElectricSQL, NATS, Postgres)

**BYTES** provides reactive streaming for files and binary data:
- Chunk-based processing
- Cloud object storage (NATS Object Store, S3)
- Transparent streaming

**Key insight**: Variables ARE the database - storage happens automatically via runtime configuration, with field-level reactivity provided by backends like ElectricSQL.

---

## Table of Contents

1. [Why TABLE (Not MAP or STORE)](#why-table-not-map-or-store)
2. [TABLE Syntax](#table-syntax)
3. [Hardware Implementation](#hardware-implementation)
4. [Software Implementation](#software-implementation)
5. [Backend Configuration](#backend-configuration)
6. [Field-Level Reactivity](#field-level-reactivity)
7. [BYTES Streaming](#bytes-streaming)
8. [Complete Examples](#complete-examples)
9. [Research Summary](#research-summary)

---

## Why TABLE (Not MAP or STORE)

### Name Conflict: MAP

```boon
users: MAP { UserId, User }        -- ❌ MAP as noun (collection)
items |> List/map(old, new: ...)   -- map as verb (transform)
```

**Problem**: `map` already used as verb in `List/map()` - confusing!

### Name Conflict: STORE

```boon
store: [                           -- ❌ store already used for state!
    users: STORE { UserId, User }  -- STORE as collection type
    counter: 0
]
```

**Problem**: `store` commonly used for global state object in Boon apps.

### TABLE: Clear and Unambiguous ✅

```boon
users: TABLE { UserId, User }      -- ✅ Clear database connotation
items |> List/map(old, new: ...)   -- ✅ No conflict!

store: [                           -- ✅ No conflict!
    users: TABLE { UserId, User }
    sessions: TABLE { SessionId, Session }
]
```

**Benefits**:
- Database/SQL connotation (perfect for Postgres backends)
- "Lookup table" terminology in hardware
- No naming conflicts
- Clear intent: tabular key→value storage

---

## TABLE Syntax

### Dynamic TABLE (Software-Only)

```boon
-- No size specified = dynamic (unlimited)
users: TABLE { UserId, User }

-- Inserts
users |> Table/insert(key: user.id, value: user)

-- Gets (returns Option)
user: users |> Table/get(key: user_id) |> WHEN {
    Some[u] => u
    None => default_user
}

-- Removes
users |> Table/remove(key: old_user_id)

-- Contains
exists: users |> Table/contains(key: user_id)  -- Bool

-- All keys/values/entries
all_ids: users |> Table/keys()       -- LIST { UserId }
all_users: users |> Table/values()   -- LIST { User }
all_entries: users |> Table/entries()  -- LIST { [key, value] }
```

### Fixed-Size TABLE (Hardware + Software)

```boon
-- Size specified = fixed-size (works in hardware!)
cache: TABLE { 16, UserId, CacheEntry }

-- Same operations
cache |> Table/insert(key: id, value: entry)
entry: cache |> Table/get(key: id)
```

**Hardware synthesis:**
- Size ≤ 16: Content-Addressable Memory (CAM)
- Size ≤ 256: Hash + Block RAM
- Known keys: Perfect Hash + Block RAM
- Ordered access: BTree + Block RAM

---

## Hardware Implementation

### Small TABLE: Content-Addressable Memory (CAM)

```boon
-- Opcode dispatch table (10 opcodes, sparse IDs)
#[hardware]
dispatch: TABLE { 16, OpCode, MicroOps }
    |> Table/insert(key: 0x00, value: nop_ops)
    |> Table/insert(key: 0x42, value: add_ops)
    |> Table/insert(key: 0x91, value: mul_ops)

ops: dispatch |> Table/get(key: instruction.opcode)
```

**Synthesizes to SystemVerilog CAM:**

```systemverilog
typedef struct {
    logic valid;
    logic [7:0] key;    // OpCode
    MicroOps value;
} CamEntry;

CamEntry cam[16];  // 16 entries

// PARALLEL comparison (all entries in 1 cycle!)
logic [15:0] matches;
for (genvar i = 0; i < 16; i++) begin
    assign matches[i] = cam[i].valid && (cam[i].key == search_key);
end

// Priority encoder selects first match - O(1) hardware!
always_comb begin
    ops = default_ops;
    for (int i = 0; i < 16; i++) begin
        if (matches[i]) begin
            ops = cam[i].value;
            break;
        end
    end
end
```

**Characteristics:**
- ✅ Parallel O(1) lookup (single cycle)
- ✅ Sparse keys (only store actual entries)
- ⚠️ Resource-intensive (comparators, registers)
- ⚠️ Practical limit: 16-64 entries

### Medium TABLE: Hash + Block RAM

```boon
-- Cache with 256 entries
#[hardware]
cache: TABLE { 256, Address, Data }
    |> Table/insert(key: addr, value: data)

cached: cache |> Table/get(key: lookup_addr)
```

**Synthesizes to Hash Table:**

```systemverilog
// Cycle 1: Compute hash
logic [7:0] hash;
assign hash = lookup_addr % 256;

// Cycle 2-3: Read from BRAM (2 cycle latency)
logic [31:0] bram_data;
always_ff @(posedge clk) begin
    bram_data <= cache_bram[hash];
end

// Cycle 4: Compare key (handle collisions)
logic match;
assign match = (bram_data.key == lookup_addr);
assign cached = match ? bram_data.value : default_value;
```

**Characteristics:**
- ✅ Large capacity (4K-64K entries)
- ✅ Uses cheap Block RAM
- ✅ Scalable
- ❌ Multi-cycle latency (3-5 cycles)
- ❌ Collision handling needed

### Large TABLE: BTree + Block RAM (Ordered)

```boon
-- Sorted lookup table
#[hardware]
sorted_lut: TABLE { 1024, sorted: True, Key, Value }

value: sorted_lut |> Table/get(key: search_key)
```

**Synthesizes to Binary Search:**

```systemverilog
// Binary search in BRAM: log2(1024) = 10 cycles
logic [9:0] low, high, mid;
logic [31:0] result;

always_ff @(posedge clk) begin
    if (start) begin
        low <= 0;
        high <= 1023;
    end else if (low <= high) begin
        mid <= (low + high) / 2;
        if (bram[mid].key == search_key) begin
            result <= bram[mid].value;
            done <= 1;
        end else if (bram[mid].key < search_key) begin
            low <= mid + 1;
        end else begin
            high <= mid - 1;
        end
    end
end
```

**Characteristics:**
- ✅ Ordered iteration (keys sorted)
- ✅ Predictable latency (log N cycles)
- ✅ Efficient BRAM usage
- ❌ Multi-cycle (10 cycles for 1K entries)
- ✅ Range queries possible

### Perfect Hash (Compile-Time Known Keys)

```boon
-- All keys known at compile time
#[hardware]
FUNCTION instruction_decode(opcode) {
    BLOCK {
        -- Compiler generates perfect hash function
        -- (no collisions possible!)
        handlers: TABLE { 32, OpCode, Handler }
            |> Table/insert(key: ADD, value: add_handler)
            |> Table/insert(key: SUB, value: sub_handler)
            // ... all 32 opcodes

        handler: handlers |> Table/get(key: opcode)
        [handler: handler]
    }
}
```

**Compiler:**
1. Analyzes all keys at compile time
2. Generates perfect hash function (no collisions!)
3. Creates minimal BRAM lookup table
4. Single BRAM read (1-2 cycles)

**Characteristics:**
- ✅ Optimal (O(1), no collisions)
- ✅ Minimal BRAM usage
- ✅ Fast (1-2 cycles)
- ⚠️ Only works when keys known at compile time

### Compiler Strategy (Automatic Selection)

```
Input: TABLE { size?, Key, Value }

If keys known at compile-time:
    → Perfect Hash + BRAM (optimal)

Else if size ≤ 16:
    → CAM (parallel, expensive, 1 cycle)

Else if size ≤ 256:
    → Hash + BRAM (cheap, 3-5 cycles)

Else if ordered: True:
    → BTree + BRAM (log N cycles)

Else:
    → Hash + BRAM (default)
```

**User can override:**

```boon
#[hardware.table_impl = "cam"]
small_cache: TABLE { 32, Key, Value }  -- Force CAM

#[hardware.table_impl = "btree"]
ordered: TABLE { 1024, Key, Value }  -- Force BTree
```

---

## Software Implementation

### Dynamic Hash Table (Default)

```boon
-- No size constraint in software
users: TABLE { UserId, User }
    |> Table/insert(key: user.id, value: user)
    // ... millions of users

user: users |> Table/get(key: user_id)
```

**Rust implementation:**

```rust
struct Table<K, V> {
    map: HashMap<K, Mutable<V>>,  // Each value is reactive!
}

impl<K: Hash + Eq, V: Clone> Table<K, V> {
    fn insert(&self, key: K, value: V) {
        if let Some(cell) = self.map.get(&key) {
            cell.set(value);  // Update existing (triggers reactivity)
        } else {
            self.map.insert(key, Mutable::new(value));  // New entry
        }
    }

    fn get(&self, key: &K) -> Signal<Option<V>> {
        if let Some(cell) = self.map.get(key) {
            cell.signal().map(Some)  // Reactive signal
        } else {
            signal::always(None)  // Constant None signal
        }
    }
}
```

**Reactivity:**
- Per-key reactivity (like MEMORY)
- Inserting key X only updates subscribers of key X
- Other keys unaffected

### BTreeMap (Ordered)

```boon
-- Ordered TABLE for range queries
sorted_users: TABLE { sorted: True, UserId, User }

-- Range query
recent_users: sorted_users
    |> Table/range(from: start_id, to: end_id)  -- LIST { User }
```

**Rust implementation:**

```rust
struct OrderedTable<K, V> {
    map: BTreeMap<K, Mutable<V>>,  // Ordered!
}

impl<K: Ord, V> OrderedTable<K, V> {
    fn range(&self, from: K, to: K) -> Vec<V> {
        self.map.range(from..=to)
            .map(|(_, cell)| cell.get())
            .collect()
    }
}
```

---

## Backend Configuration

### Configuration in Boon (Not TOML!)

All configuration is Boon code:

```boon
-- config/backends.bn

-- LocalStorage backend (browser)
local_storage: Backend/local_storage(
    prefix: TEXT { my_app }
)

-- ElectricSQL backend (Postgres with cell-level sync)
postgres: Backend/electric_sql(
    url: Env/var(TEXT { DATABASE_URL })
    sync_granularity: Cell  -- Field-level reactivity!
)

-- NATS KV backend (distributed key-value)
nats_kv: Backend/nats_kv(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { sessions }
    ttl: minutes(30)  -- Auto-expire after 30 minutes
)

-- NATS Object Store backend (file storage)
nats_objects: Backend/nats_object_store(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { user_files }
)

-- S3 backend (cloud storage)
s3: Backend/s3(
    region: TEXT { us-east-1 }
    bucket: TEXT { my-app-files }
    credentials: Env/aws_credentials()
)
```

### Module-Level Backend Assignment

```boon
-- config/modules.bn

-- Default backend for all variables
default_backend: local_storage

-- Per-module backends
module_backends: [
    UserStore: postgres       -- Uses ElectricSQL
    SessionStore: nats_kv     -- Uses NATS KV
    FileStore: nats_objects   -- Uses NATS Object Store
]
```

### Variable-Level Backend Override

```boon
-- users.bn

MODULE UserStore

-- Uses postgres backend (from module config)
users: TABLE { UserId, User }
    |> Table/insert(key: new_user.id, value: new_user)

-- Override for specific variable
#[backend: nats_kv]
active_sessions: TABLE { UserId, SessionId }
    |> Table/insert(key: user_id, value: session_id)
```

### Environment Variables

```boon
-- config/env.bn

-- Development
#[env: development]
database_url: TEXT { postgresql://localhost/dev_db }
nats_url: TEXT { nats://localhost:4222 }

-- Production
#[env: production]
database_url: Env/var(TEXT { DATABASE_URL })
nats_url: Env/var(TEXT { NATS_URL })
```

---

## Field-Level Reactivity

### The Problem: Whole-Object Updates

**Without field-level reactivity:**

```boon
users: TABLE { UserId, User }
-- User: [surname: Text, age: Number, email: Text]

user: users |> Table/get(key: user_id)
surname: user |> WHEN { Some[u] => u.surname, None => Text/empty }

-- When ANY field changes (age, email, surname):
-- 1. Entire User object re-serialized
-- 2. Entire User object sent over network
-- 3. user signal updates
-- 4. surname re-evaluates (even if surname unchanged!)
```

**Inefficient!** Changing `age` shouldn't re-evaluate `surname` subscribers.

### The Solution: ElectricSQL Cell-Level Sync

[**ElectricSQL**](https://electric-sql.com) (2024) provides **cell-level reactive updates** for Postgres!

> "dramatically simplified architecture while delivering the performance needed for **cell-level reactive updates**"

**Production use:** Otto's AI spreadsheet - EVERY CELL is a reactive agent!

### How It Works

**Backend config:**

```boon
-- config/backends.bn

postgres: Backend/electric_sql(
    url: Env/var(TEXT { DATABASE_URL })
    sync_granularity: Cell  -- ✅ Field-level reactivity!
)
```

**Boon code (no changes needed!):**

```boon
users: TABLE { UserId, User }

user: users |> Table/get(key: user_id)

-- Only updates when surname changes!
surname: user |> WHEN {
    Some[u] => u.surname
    None => Text/empty
}

-- Only updates when age changes!
age: user |> WHEN {
    Some[u] => u.age
    None => 0
}
```

**What happens:**

1. **User changes age in Postgres:**
   ```sql
   UPDATE users SET age = 31 WHERE id = '...';
   ```

2. **ElectricSQL detects cell-level change:**
   - Tracks which cell changed (column `age`, row `user_id`)
   - Streams ONLY that cell update (not whole row!)

3. **Boon runtime receives update:**
   - Updates local SQLite (ElectricSQL local cache)
   - Notifies only `age` subscribers
   - `surname` subscribers NOT notified! ✅

4. **Reactivity:**
   - `age` signal updates
   - `surname` signal does NOT update (different cell!)

### Nested Fields

```boon
User: [
    name: [first: Text, last: Text]
    contact: [email: Text, phone: Text]
]

users: TABLE { UserId, User }
user: users |> Table/get(key: user_id)

-- Only updates when first name changes!
first_name: user |> WHEN {
    Some[u] => u.name.first
    None => Text/empty
}

-- Only updates when email changes!
email: user |> WHEN {
    Some[u] => u.contact.email
    None => Text/empty
}
```

**ElectricSQL handles nested paths:**
- Postgres column: `contact` (JSONB)
- ElectricSQL tracks: `contact.email` vs `contact.phone` separately
- Updates stream at JSON path granularity

### Alternative: Explicit Field Tables

If backend doesn't support field-level sync, decompose manually:

```boon
-- Separate tables for different fields
user_names: TABLE { UserId, Text }
user_ages: TABLE { UserId, Number }
user_emails: TABLE { UserId, Text }

-- Subscribe to specific field
surname: user_names |> Table/get(key: user_id)
age: user_ages |> Table/get(key: user_id)
```

**Pros:**
- ✅ Explicit field-level granularity
- ✅ Works with any backend

**Cons:**
- ❌ Verbose (many tables)
- ❌ No "whole user" object
- ❌ Manual decomposition

**Recommendation:** Use ElectricSQL for automatic field-level reactivity!

---

## BYTES Streaming

### Problem: Large Binary Data

**Traditional approach:**

```boon
-- ❌ Load entire 100MB file into memory
video: Bytes/read_file(path: TEXT { /data/video.mp4 })
// Out of memory! Slow! Blocking!
```

**Streaming approach:**

```boon
-- ✅ Process 1MB chunks as they arrive
video: BYTES {}
    |> Bytes/stream_from_file(path: TEXT { /data/video.mp4 })
    |> Bytes/stream_chunks(size: megabytes(1))
    |> Bytes/process_chunk(chunk, process: encode_chunk(chunk))
// Fast! Low memory! Reactive!
```

### BYTES Syntax

```boon
-- Empty BYTES (for streaming)
data: BYTES {}

-- BYTES from literal (compile-time)
small_data: BYTES { 0x48, 0x65, 0x6C, 0x6C, 0x6F }  -- "Hello"

-- BYTES from text
text_bytes: TEXT { Hello, world! } |> Text/to_bytes(encoding: UTF8)
```

### Streaming from File

```boon
-- Stream large file (reactive chunks)
video: BYTES {}
    |> Bytes/stream_from_file(path: TEXT { /data/video.mp4 })

-- Process chunks as they arrive
video |> Bytes/stream_chunks(size: megabytes(1)) |> THEN { chunk =>
    chunk |> process_chunk()
}
```

### Streaming to File

```boon
-- Stream to file (incremental writes)
output: BYTES {}
    |> Bytes/stream_to_file(path: TEXT { /output/encoded.mp4 })

-- Write chunks
download_stream |> THEN { chunk =>
    output |> Bytes/write_chunk(chunk)
}
```

### NATS Object Store

[**NATS Object Store**](https://docs.nats.io/nats-concepts/jetstream/obj_store) provides distributed file storage with streaming and reactivity.

**Backend config:**

```boon
-- config/backends.bn

nats_objects: Backend/nats_object_store(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { user_files }
)
```

**Upload with streaming:**

```boon
-- Upload user avatar
avatar: BYTES {}
    |> Bytes/stream_from_file(path: avatar_file_path)
    |> Bytes/save_to_store(
        backend: nats_objects
        key: TEXT { avatars/{user_id} }
    )

-- Automatically chunked into JetStream messages
-- Progress updates via reactive signals
upload_progress: avatar.upload_percent  -- 0.0 to 1.0
```

**Download with streaming:**

```boon
-- Download avatar (reactive!)
avatar_data: BYTES {}
    |> Bytes/stream_from_store(
        backend: nats_objects
        key: TEXT { avatars/{user_id} }
    )

-- Convert to data URL for <img> element
avatar_url: avatar_data
    |> Bytes/to_data_url(mime_type: TEXT { image/jpeg })

-- Use in UI
avatar_image: Element/image(
    src: avatar_url  -- Updates when file changes in store!
)
```

**Watch for changes (reactive!):**

```boon
-- Subscribe to file changes
avatar_data: BYTES {}
    |> Bytes/stream_from_store(
        backend: nats_objects
        key: TEXT { avatars/{user_id} }
        watch: True  -- Watch for updates!
    )

-- When OTHER client uploads new avatar:
-- 1. NATS Object Store notifies all watchers
-- 2. Boon runtime streams new file
-- 3. avatar_data updates
-- 4. avatar_url regenerates
-- 5. UI updates automatically!
```

### S3 / Cloudflare R2

**Backend config:**

```boon
s3: Backend/s3(
    region: TEXT { us-east-1 }
    bucket: TEXT { my-app-files }
    credentials: Env/aws_credentials()
)
```

**Streaming upload:**

```boon
-- Multipart upload for large files
large_video: BYTES {}
    |> Bytes/stream_from_file(path: video_path)
    |> Bytes/save_to_store(
        backend: s3
        key: TEXT { videos/{video_id}.mp4 }
        multipart: True  -- Use multipart upload
        chunk_size: megabytes(5)  -- 5MB parts
    )

upload_progress: large_video.upload_percent
```

**Streaming download:**

```boon
-- Range requests for partial downloads
video_chunk: BYTES {}
    |> Bytes/stream_from_store(
        backend: s3
        key: TEXT { videos/{video_id}.mp4 }
        range: [
            from: megabytes(10)  -- Start at 10MB
            to: megabytes(20)    -- End at 20MB
        ]
    )
```

### Progressive Processing

```boon
-- Encode video while uploading
encoded_video: BYTES {}
    |> Bytes/stream_from_file(path: raw_video_path)
    |> Bytes/stream_chunks(size: megabytes(1))
    |> Bytes/transform_chunk(chunk, transform: encode_h264(chunk))
    |> Bytes/save_to_store(
        backend: s3
        key: TEXT { videos/encoded/{video_id}.mp4 }
    )

-- Progress tracking
encoding_progress: encoded_video.transform_percent  -- 0.0 to 1.0
upload_progress: encoded_video.upload_percent       -- 0.0 to 1.0
```

### In-Memory Chunks (Hardware)

```boon
#[hardware]
FUNCTION stream_processor(data_stream) {
    BLOCK {
        -- Buffer for incoming chunks
        buffer: MEMORY { 1024, BITS { 8, 2u0 } }
            |> Memory/write_entry(entry: data_stream |> THEN {
                [address: write_ptr, data: data]
            })

        -- Pointers
        write_ptr: 0 |> LATEST wr {
            data_stream |> THEN { (wr + 1) % 1024 }
        }

        read_ptr: 0 |> LATEST rd {
            process_complete |> THEN { (rd + 1) % 1024 }
        }

        -- Stream output
        chunk: buffer |> Memory/read(address: read_ptr)

        [chunk: chunk]
    }
}
```

---

## Complete Examples

### Example 1: Web App with Users (ElectricSQL)

**Backend config:**

```boon
-- config/backends.bn

postgres: Backend/electric_sql(
    url: Env/var(TEXT { DATABASE_URL })
    sync_granularity: Cell  -- Field-level reactivity
)

nats_kv: Backend/nats_kv(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { sessions }
    ttl: minutes(30)
)
```

**Users module:**

```boon
-- modules/users.bn

MODULE UserStore

-- Automatically synced with Postgres via ElectricSQL
users: TABLE { UserId, User }
    |> Table/insert_entry(entry: signup_event |> THEN {
        [key: new_user.id, value: new_user]
    })
    |> Table/remove_entry(key: delete_user_event)

-- Get current user (reactive!)
current_user: users |> Table/get(key: current_user_id) |> WHEN {
    Some[user] => user
    None => redirect_to_login
}

-- Field-level subscriptions (only update on field change!)
user_name: current_user.name        -- Only updates when name changes
user_email: current_user.email      -- Only updates when email changes
user_avatar: current_user.avatar    -- Only updates when avatar changes
```

**Sessions module:**

```boon
-- modules/sessions.bn

MODULE SessionStore

-- Automatically synced with NATS KV (distributed)
#[backend: nats_kv]
sessions: TABLE { SessionId, Session }
    |> Table/insert_entry(entry: login_event |> THEN {
        [
            key: session_id
            value: [user_id: user_id, expires: now() + hours(24)]
        ]
    })
    |> Table/remove_entry(key: logout_event)

-- Get current session
current_session: sessions |> Table/get(key: request.session_id)

-- Verify session
authenticated: current_session |> WHEN {
    Some[session] => session.expires > now()
    None => False
}
```

**When other server updates user email:**
1. PostgreSQL: `UPDATE users SET email = 'new@email.com' WHERE id = '...'`
2. ElectricSQL detects cell change (column `email`, row `user_id`)
3. Streams ONLY email cell update (not whole user!)
4. This server's ElectricSQL client receives update
5. `user_email` signal updates
6. `user_name` does NOT update (different cell!)

### Example 2: File Upload (NATS Object Store)

```boon
-- file_upload.bn

MODULE FileStore

-- NATS Object Store for files
#[backend: nats_objects]
user_files: Backend/nats_object_store(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { user_uploads }
)

-- Upload file with progress
uploaded_file: BYTES {}
    |> Bytes/stream_from_file(path: selected_file.path)
    |> Bytes/save_to_store(
        backend: user_files
        key: TEXT { users/{user_id}/files/{file_id} }
    )

-- Progress bar
upload_progress: uploaded_file.upload_percent

progress_bar: Element/progress(
    value: upload_progress
    max: 1.0
    label: TEXT { Uploading... {upload_progress * 100}% }
)

-- Download file (reactive!)
downloaded_file: BYTES {}
    |> Bytes/stream_from_store(
        backend: user_files
        key: TEXT { users/{user_id}/files/{file_id} }
        watch: True  -- Watch for changes!
    )

-- Display image
file_url: downloaded_file |> Bytes/to_data_url(
    mime_type: file.mime_type
)

image_element: Element/image(
    src: file_url  -- Updates when file changes!
)
```

### Example 3: Hardware Instruction Dispatch (CAM)

```boon
-- cpu_dispatch.bn

#[hardware]
FUNCTION instruction_dispatch(opcode, operands) {
    BLOCK {
        -- 32-entry CAM for sparse opcode lookup
        dispatch_table: TABLE { 32, OpCode, MicroOps }
            |> Table/insert(key: 0x00, value: NOP)
            |> Table/insert(key: 0x01, value: ADD)
            |> Table/insert(key: 0x02, value: SUB)
            |> Table/insert(key: 0x10, value: MUL)
            |> Table/insert(key: 0x11, value: DIV)
            // ... 27 more opcodes

        -- Parallel lookup (1 cycle in hardware!)
        microops: dispatch_table |> Table/get(key: opcode) |> WHEN {
            Some[ops] => ops
            None => ILLEGAL_INSTRUCTION
        }

        [microops: microops, valid: microops != ILLEGAL_INSTRUCTION]
    }
}
```

**Hardware synthesis:**
- 32-entry CAM (all keys compared in parallel)
- O(1) lookup (single cycle)
- Only stores 32 actual entries (sparse)
- Resource cost: ~32 comparators + storage

### Example 4: State Migration (Across Backends)

**Version 1:**

```boon
-- v1: LocalStorage backend
counter: 0 |> LATEST count {
    increment_button.event.press |> THEN { count + 1 }
}
```

**Version 2: Migrate to Postgres**

```boon
-- config/backends.bn (new config)
postgres: Backend/electric_sql(
    url: Env/var(TEXT { DATABASE_URL })
)

-- v2: Migrate to Postgres, increment by 2
#[backend: postgres]
counter_v2: LATEST count {
    counter  -- ← Migrate from v1 (LocalStorage) to v2 (Postgres)
    increment_button.event.press |> THEN { count + 2 }
}
```

**What happens:**
1. Runtime loads `counter` from LocalStorage (v1)
2. First value flows through LATEST: `counter_v2 = counter`
3. `counter_v2` saved to Postgres (new backend)
4. LocalStorage → Postgres migration complete!
5. Future updates go to Postgres only

**Version 3: Remove old counter**

```boon
-- v3: Clean up
#[backend: postgres]
counter_v2: 0 |> LATEST count {
    increment_button.event.press |> THEN { count + 2 }
}
```

**Migration done!** Old `counter` in LocalStorage can be cleared.

### Example 5: Video Processing Pipeline

```boon
-- video_pipeline.bn

MODULE VideoProcessor

-- S3 backend for videos
s3: Backend/s3(
    region: TEXT { us-east-1 }
    bucket: TEXT { video-processing }
)

-- Upload raw video with progress
raw_video: BYTES {}
    |> Bytes/stream_from_file(path: upload.file_path)
    |> Bytes/save_to_store(
        backend: s3
        key: TEXT { raw/{video_id}.mp4 }
        multipart: True
        chunk_size: megabytes(5)
    )

upload_progress: raw_video.upload_percent

-- Process: transcode while uploading
encoded_video: BYTES {}
    |> Bytes/stream_from_store(
        backend: s3
        key: TEXT { raw/{video_id}.mp4 }
    )
    |> Bytes/stream_chunks(size: megabytes(1))
    |> Bytes/transform_chunk(chunk, transform:
        chunk |> transcode_h264(
            bitrate: megabits(5)
            resolution: [width: 1920, height: 1080]
        )
    )
    |> Bytes/save_to_store(
        backend: s3
        key: TEXT { encoded/{video_id}.mp4 }
    )

transcode_progress: encoded_video.transform_percent
encoding_upload_progress: encoded_video.upload_percent

-- Generate thumbnail (from first frame)
thumbnail: encoded_video
    |> Bytes/take_first_chunk()
    |> Bytes/extract_frame(time: seconds(0))
    |> Bytes/resize(width: 320, height: 180)
    |> Bytes/save_to_store(
        backend: s3
        key: TEXT { thumbnails/{video_id}.jpg }
    )

-- UI
progress_ui: Element/stripe(
    direction: Column
    items: LIST {
        Element/progress(
            label: TEXT { Uploading... {upload_progress * 100}% }
            value: upload_progress
        )
        Element/progress(
            label: TEXT { Encoding... {transcode_progress * 100}% }
            value: transcode_progress
        )
    }
)
```

---

## Research Summary

### Reactive Databases Evaluated

#### **ElectricSQL** ⭐ RECOMMENDED

- **URL**: https://electric-sql.com
- **Status**: Production (Beta Dec 2024, v1.0 Mar 2025)
- **Key Feature**: **Cell-level reactive updates** for Postgres
- **Granularity**: Field/column level (finest possible!)
- **Use Case**: Otto AI spreadsheet (every cell = reactive agent)
- **Performance**: Scales to 80Gb/s, 1M concurrent users
- **Integration**: Postgres ↔ SQLite sync (local-first)

**Why it's perfect for Boon:**
- ✅ Automatic cell-level reactivity (no manual field decomposition)
- ✅ Postgres backend (production-ready, SQL capabilities)
- ✅ Local SQLite cache (offline-first, fast reads)
- ✅ Real-time sync (changes stream immediately)
- ✅ Open source (MIT license)

**Example from research:**
> "dramatically simplified architecture while delivering the performance needed for **cell-level reactive updates**"

Production companies using it: Google, Supabase, Trigger.dev, Otto, Doorboost

#### **RxDB**

- **URL**: https://rxdb.info
- **Status**: Production
- **Key Feature**: Field-level subscriptions via `observeWithColumns()`
- **Granularity**: Field level (specify which columns to watch)
- **Use Case**: Local-first apps, React/React Native
- **Integration**: CouchDB, GraphQL sync

**Pros:**
- ✅ Field-level reactivity
- ✅ Local-first (works offline)
- ✅ RxJS-based (reactive by design)

**Cons:**
- ❌ Manual field specification needed
- ❌ CouchDB not as common as Postgres

#### **WatermelonDB**

- **URL**: https://watermelondb.dev
- **Status**: Production
- **Key Feature**: Field-level via `observeWithColumns()`
- **Granularity**: Field level
- **Use Case**: React Native (SQLite backend)
- **Performance**: Lazy loading, separate native thread

**Pros:**
- ✅ Field-level reactivity
- ✅ Fast (native SQLite)
- ✅ Offline-first

**Cons:**
- ❌ Mobile-focused (React Native primarily)
- ❌ Manual column specification

#### **NATS JetStream**

- **URL**: https://docs.nats.io
- **Status**: Production
- **Key Feature**: KV Store with Watch, Object Store for files
- **Granularity**: Key level (not field level)
- **Use Case**: Distributed systems, microservices

**Strengths:**
- ✅ KV Watch (reactive updates when keys change)
- ✅ Object Store (streaming large files)
- ✅ Distributed, highly scalable
- ✅ TTL support (auto-expire keys)

**Limitations:**
- ❌ No field-level granularity (whole value updates)
- ❌ Not a full database (no SQL, joins, etc.)

**Best for:** Sessions, caching, file storage (not primary data)

#### **RethinkDB**

- **URL**: https://rethinkdb.com
- **Status**: Open source (community maintained)
- **Key Feature**: Changefeeds (real-time push updates)
- **Granularity**: Document/row level (not field level)
- **Use Case**: Real-time apps

**Pros:**
- ✅ Built-in real-time (changefeeds)
- ✅ Query-level subscriptions

**Cons:**
- ❌ No field-level granularity
- ❌ Less active development

#### **Datomic**

- **URL**: https://www.datomic.com
- **Status**: Commercial (Cognitect/Nubank)
- **Key Feature**: Immutable, temporal (time-travel queries)
- **Granularity**: Fact level (append-only)
- **Use Case**: Audit trails, temporal modeling

**Strengths:**
- ✅ Immutable (perfect audit trail)
- ✅ Time-travel ("what was value at timestamp?")
- ✅ Bitemporal (valid time + transaction time)

**Why NOT for Boon:**
- ❌ Not reactive (query-based, not streaming)
- ❌ No push updates (client must poll)
- ❌ Datalog queries (complex)
- ❌ JVM-only (Clojure)

**Verdict:** Wrong fit for Boon's reactive model.

### Hardware Synthesis Research

#### **CAM in FPGAs**

**Resource Cost** (16-entry, 32-bit key CAM):
- 16 × 32-bit comparators = 512 LUT comparisons
- Priority encoder (16→4)
- Storage: 16 × (32-bit key + value)
- **Total**: ~600-1000 LUTs for 16 entries

**Practical Limits:**
- Small FPGAs: 8-16 entries max
- Medium FPGAs: 32-64 entries max
- Large FPGAs: 128+ entries (but expensive!)

**When to use:**
- Sparse lookup (few entries, wide key space)
- Need O(1) single-cycle lookup
- Small number of entries (≤32)

#### **Hash + Block RAM**

**Resource Cost** (256-entry hash table):
- Hash function: ~50-100 LUTs
- Block RAM: 1-2 BRAM blocks (256 × entry_size)
- Collision logic: ~100 LUTs
- **Total**: ~200 LUTs + 1-2 BRAMs (cheap!)

**Characteristics:**
- ✅ Large capacity (BRAM is plentiful)
- ✅ Low LUT cost
- ❌ Multi-cycle (3-5 cycles)
- ❌ Collision handling needed

**When to use:**
- Large tables (>64 entries)
- Multi-cycle acceptable
- BRAM available

#### **BTree in Hardware**

**Resource Cost** (1024-entry binary search):
- BRAM: 1-2 blocks (for sorted data)
- Binary search FSM: ~200 LUTs
- Log2(1024) = 10 cycles latency
- **Total**: ~200 LUTs + 1-2 BRAMs

**When to use:**
- Need ordered iteration
- Range queries
- Predictable latency (log N)

#### **Perfect Hash**

**Compiler-Generated** (when keys known):
- Minimal perfect hash function (MPHF)
- No collisions by construction
- Single BRAM lookup (1-2 cycles)
- Optimal space (no wasted slots)

**When to use:**
- Keys known at compile time
- Need optimal performance
- Worth compile-time cost

### BYTES Streaming Research

#### **NATS Object Store**

**Features:**
- Chunked storage (splits large files)
- Streaming upload/download
- Watch API (reactive file changes!)
- Metadata support
- Link to external URLs

**Perfect for:**
- User-uploaded files (avatars, documents)
- Video/audio streaming
- Distributed file storage
- Reactive file updates

**Limitations:**
- Not optimized for massive files (use S3 for huge videos)
- Requires NATS server

#### **S3 / Cloudflare R2**

**Features:**
- Massive storage (exabytes)
- Multipart upload (>5GB files)
- Range requests (partial downloads)
- CDN integration
- Low cost (R2 has no egress fees!)

**Perfect for:**
- Long-term storage
- Large files (videos, datasets)
- Public assets (CDN)
- Cost-effective archives

**Limitations:**
- No built-in reactivity (need polling or webhooks)
- Network latency

### Comparison Matrix

| Backend | Type | Granularity | Reactivity | Streaming | Best For |
|---------|------|-------------|------------|-----------|----------|
| **ElectricSQL** | Postgres | ✅ Cell | ✅ Real-time | ❌ No | **Primary data** |
| **NATS KV** | Key-Value | ⚠️ Key | ✅ Watch | ❌ No | Sessions, cache |
| **NATS Object** | Files | ⚠️ File | ✅ Watch | ✅ Yes | User files |
| **S3/R2** | Object Store | ⚠️ File | ❌ No | ✅ Yes | Large files |
| **LocalStorage** | Browser | ⚠️ Key | ❌ No | ❌ No | Browser state |
| **RxDB** | Local DB | ✅ Field | ✅ RxJS | ❌ No | Mobile apps |

---

## Recommendations

### For Server Apps: ElectricSQL + NATS

```boon
-- Primary data: ElectricSQL (cell-level reactivity!)
postgres: Backend/electric_sql(
    url: Env/var(TEXT { DATABASE_URL })
    sync_granularity: Cell
)

-- Sessions/cache: NATS KV (distributed, TTL)
nats_kv: Backend/nats_kv(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { sessions }
    ttl: minutes(30)
)

-- Files: NATS Object Store (streaming, reactive)
nats_objects: Backend/nats_object_store(
    url: Env/var(TEXT { NATS_URL })
    bucket: TEXT { uploads }
)

-- Large files: S3 (cost-effective, scalable)
s3: Backend/s3(
    bucket: TEXT { archives }
)
```

### For Browser Apps: LocalStorage

```boon
-- Already works!
local: Backend/local_storage(
    prefix: TEXT { my_app }
)
```

### For Hardware: TABLE with CAM/Hash/BTree

```boon
-- Small: CAM (parallel)
#[hardware]
small: TABLE { 16, Key, Value }

-- Medium: Hash + BRAM
#[hardware]
medium: TABLE { 256, Key, Value }

-- Ordered: BTree
#[hardware]
large: TABLE { 1024, sorted: True, Key, Value }
```

---

## Open Questions

1. **Conflict resolution**: When two clients update same key simultaneously?
   - ElectricSQL uses CRDTs/OT for automatic merging
   - NATS KV uses "last write wins" or compare-and-set
   - Need Boon-level API for conflict handling?

2. **Schema evolution**: How to handle field additions/removals?
   - ElectricSQL supports schema migrations
   - Need Boon migration syntax?

3. **Offline behavior**: What happens when backend unreachable?
   - ElectricSQL: Local SQLite cache continues working
   - NATS: Buffers updates, replays on reconnect
   - LocalStorage: Always available
   - Need explicit offline/online state in Boon?

4. **Type safety**: Ensure TABLE { K, V } types match backend schemas?
   - Generate Postgres schemas from Boon types?
   - Or infer Boon types from existing schemas?

5. **BYTES size limits**: Max file size for different backends?
   - NATS Object Store: Configurable (default 1GB per object)
   - S3: 5TB per object
   - Need size validation in Boon?

---

## Next Steps

1. **Prototype TABLE in browser** (LocalStorage backend)
   - Implement in playground runtime
   - Test reactivity, migrations

2. **Integrate ElectricSQL**
   - Rust client library
   - Cell-level subscription API
   - Test field-level reactivity

3. **BYTES streaming POC**
   - Chunk-based processing
   - Progress tracking
   - NATS Object Store integration

4. **Hardware synthesis**
   - CAM generation (small TABLE)
   - Hash + BRAM (medium TABLE)
   - Test in FPGA simulator

5. **Documentation**
   - User guide (TABLE operations)
   - Backend configuration guide
   - Migration patterns

---

**TABLE + BYTES: Making storage and streaming transparent, reactive, and universal across all Boon domains!** 🚀
