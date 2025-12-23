## Part 6: FLUSH Error Handling

### 6.1 FLUSH in Message Payload

FLUSH creates a `Flushed` wrapper that propagates through pipelines.

**See Part 3.1 for canonical Payload definition** - includes `Flushed(Box<Payload>)` variant.

### 6.2 FLUSH Semantics

When `FLUSH { error }` executes:
1. **Local Exit** - Exits current pipeline expression immediately
2. **Wrapper Creation** - Creates `Flushed(error)` payload
3. **Transparent Propagation** - Bypasses all downstream nodes until boundary

### 6.3 Node Handling of Flushed

Every node checks for Flushed wrapper:

```rust
impl ReactiveNode {
    fn process_message(&mut self, msg: Message) -> Option<Message> {
        // Check for Flushed - bypass if present
        if let Payload::Flushed(_) = &msg.payload {
            return Some(msg);  // Pass through unchanged
        }
        // Normal processing...
        self.process_normal(msg)
    }
}
```

### 6.4 Boundary Unwrapping

`Flushed(value)` unwraps to `value` at:
- Variable bindings (assignment completes)
- Function returns
- BLOCK returns

### 6.5 List/map Fail-Fast

When List/map encounters Flushed in iteration:
```rust
impl Bus {
    fn map_item(&mut self, item: Message) -> Option<Message> {
        if let Payload::Flushed(_) = &item.payload {
            // Stop processing remaining items
            self.abort_iteration();
            return Some(item);  // Return Flushed to caller
        }
        // Process item...
    }
}
```

---

