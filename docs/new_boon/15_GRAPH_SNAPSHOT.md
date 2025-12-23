## Part 5: Full Graph Snapshot

### 5.1 Serializable State

All runtime state must be serializable.

**See K9 for canonical GraphSnapshot definition** (includes pending_messages, pending_dom_events, transport_state).

**See K24 for canonical NodeSnapshot definition** (includes subscribers, kind-specific state via NodeStateSnapshot).

### 5.2 Snapshot/Restore API

```rust
impl EventLoop {
    pub fn snapshot(&self) -> GraphSnapshot { ... }
    pub fn restore(&mut self, snapshot: GraphSnapshot) { ... }
}
```

**Files to modify:**
- `crates/boon/src/platform/browser/engine.rs` - Snapshot types and methods

---

