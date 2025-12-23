## Part 12: Native Testing & Debugging Infrastructure

Enable running `.bn` files as tests without requiring a browser, with controllable time, mock storage, and captured output for assertions.

### 12.1 Current Testing State

| Category | Status | Count |
|----------|--------|-------|
| Parser tests | ✅ Native | 30 tests |
| Browser example tests | ✅ Automated | 7+ examples |
| Evaluator tests | ❌ Missing | 0 (requires browser) |
| Engine tests | ❌ Missing | 0 (requires browser) |

### 12.2 Test Platform Components

```rust
/// Test platform for native testing
pub struct TestPlatform {
    clock: Arc<TestClock>,
    storage: MockStorage,
    renderer: MockRenderer,
    runtime: TestRuntime,
}

impl TestPlatform {
    /// Advance virtual time and process pending timers
    pub fn tick(&mut self);

    /// Advance by N milliseconds, processing all timers
    pub fn advance_ms(&mut self, ms: u64);
}
```

### 12.3 Controllable Clock

```rust
pub struct TestClock {
    current_time: AtomicU64,
    pending_timers: RefCell<BinaryHeap<TimerEntry>>,
}

impl TestClock {
    pub fn now_ms(&self) -> u64;
    pub fn advance_by(&self, ms: u64);  // Wakes timers as time passes
    pub fn register_timer(&self, delay_ms: u64, waker: Waker);
}
```

### 12.4 Mock Storage

```rust
pub struct MockStorage {
    data: RefCell<HashMap<String, JsonValue>>,
    access_log: RefCell<Vec<StorageAccess>>,  // For test assertions
}

impl MockStorage {
    pub fn with_initial(data: HashMap<String, JsonValue>) -> Self;
    pub fn access_log(&self) -> Vec<StorageAccess>;  // Verify storage calls
    pub fn snapshot(&self) -> HashMap<String, JsonValue>;  // For restart tests
}
```

### 12.5 Mock Renderer (Output Capture)

```rust
pub struct MockRenderer {
    outputs: RefCell<Vec<CapturedOutput>>,
}

impl MockRenderer {
    pub fn latest_text(&self) -> Option<String>;  // For assertions
    pub fn outputs(&self) -> Vec<CapturedOutput>;
}
```

### 12.6 BoonTest Runner

```rust
pub struct BoonTest {
    platform: TestPlatform,
    source: String,
}

impl BoonTest {
    pub fn from_source(source: &str) -> Self;
    pub fn from_file(path: &Path) -> Result<Self, Error>;

    pub fn run(&mut self) -> TestResult;
    pub fn tick(&mut self);
    pub fn advance_ms(&mut self, ms: u64);
    pub fn output(&self) -> Option<String>;
    pub fn trigger_event(&mut self, path: &str, value: Value);
    pub fn storage(&self) -> &MockStorage;
}
```

### 12.7 Assertion Macros

```rust
// Assert exact output
assert_boon_output!(test, "0");

// Assert output contains
assert_boon_output_contains!(test, "hello");

// Assert after ticks
assert_boon_output_after_ticks!(test, 5, "5");

// Fluent API
BoonAssert::new(&test)
    .output_equals("0")
    .output_contains("count")
    .output_matches(r"\d+");
```

### 12.8 Test Macros for cargo test

```rust
// Test from .bn file
boon_test!(counter, "examples/counter/counter.bn", |test| {
    test.run();
    assert_boon_output!(test, "0");

    test.trigger_event("button.event.press", Value::empty_object());
    test.tick();
    assert_boon_output!(test, "1");
});

// Test from inline code
boon_test_inline!(simple_counter, r#"
    counter: 0
    document: Document/new(root: counter)
"#, |test| {
    test.run();
    assert_boon_output!(test, "0");
});
```

### 12.9 Example Test Cases

**Counter Test:**
```rust
boon_test_inline!(counter_increments, r#"
    counter: LATEST { 0, click |> THEN { 1 } } |> Math/sum()
    click: LINK
    document: Document/new(root: counter)
"#, |test| {
    test.run();
    assert_boon_output!(test, "0");
    test.trigger_event("click", Value::empty_object());
    test.tick();
    assert_boon_output!(test, "1");
});
```

**Timer Test (Mock Clock):**
```rust
boon_test_inline!(interval_fires, r#"
    counter: 0 |> HOLD state {
        Stream/interval(ms: 1000) |> THEN { state + 1 }
    }
    document: Document/new(root: counter)
"#, |test| {
    test.run();
    assert_boon_output!(test, "0");
    test.advance_ms(1000);
    assert_boon_output!(test, "1");
    test.advance_ms(1000);
    assert_boon_output!(test, "2");
});
```

**Persistence Test:**
```rust
boon_test_inline!(persistence_restores, r#"
    counter: 0 |> HOLD state { inc |> THEN { state + 1 } }
    inc: LINK
    document: Document/new(root: counter)
"#, |test| {
    test.run();
    test.trigger_event("inc", Value::empty_object());
    test.trigger_event("inc", Value::empty_object());
    test.tick();
    assert_boon_output!(test, "2");

    // Simulate restart
    let storage = test.storage().snapshot();
    let mut test2 = BoonTest::from_source(/*same*/).with_storage(storage);
    test2.run();
    assert_boon_output!(test2, "2");  // Restored!
});
```

### 12.10 Debugging Facilities

**Tracer:**
```rust
pub struct Tracer {
    entries: RefCell<Vec<TraceEntry>>,
}

impl Tracer {
    pub fn log(&self, construct_id: &str, event: TraceEvent);
    pub fn dump(&self) -> String;  // Human-readable trace
}

pub enum TraceEvent {
    ValueEmitted { value: Value },
    ActorCreated { info: String },
    LinkTriggered { path: String },
    TimerFired { delay_ms: u64 },
}
```

**State Inspector:**
```rust
pub struct StateInspector<'a> {
    platform: &'a TestPlatform,
}

impl StateInspector {
    pub fn active_actors(&self) -> Vec<ActorState>;
    pub fn get_value(&self, path: &str) -> Option<Value>;
    pub fn dump(&self) -> String;  // Full state dump
}
```

### 12.11 Implementation Phases

#### Phase 12A: Platform Abstraction
1. Create Platform trait in platform/mod.rs
2. Extract browser code behind trait
3. Create test module skeleton

#### Phase 12B: Test Platform Core
1. Implement TestClock with manual time
2. Implement MockStorage with logging
3. Implement MockRenderer with capture

#### Phase 12C: Test Runner
1. Create BoonTest runner
2. Implement event triggering for LINK
3. Wire ActorLoop to test runtime

#### Phase 12D: Assertions & Integration
1. Create assertion macros
2. Build.rs for test discovery
3. Documentation & examples

### 12.12 Benefits

| Aspect | Before | After |
|--------|--------|-------|
| Test speed | Seconds (browser startup) | Milliseconds |
| Debugging | Browser DevTools only | Native debugger + traces |
| CI/CD | Needs headless browser | `cargo test` |
| Timer tests | Real-time waiting | Instant (mock clock) |
| Persistence | Manual browser refresh | Programmatic restart |

### 12.13 Critical Files

| File | Purpose |
|------|---------|
| `platform/mod.rs` | Platform trait abstraction |
| `platform/test/mod.rs` | **NEW:** TestPlatform |
| `platform/test/clock.rs` | **NEW:** Controllable TestClock |
| `platform/test/storage.rs` | **NEW:** MockStorage |
| `platform/test/renderer.rs` | **NEW:** MockRenderer |
| `platform/test/runner.rs` | **NEW:** BoonTest runner |
| `platform/test/assertions.rs` | **NEW:** Assertion macros |
| `build.rs` | Test discovery |
