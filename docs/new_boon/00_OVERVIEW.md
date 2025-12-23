# Boon Complete Redesign: From Reactive Runtime to RISC Softcore

## Ultimate Vision

**Design and implement a RISC softcore processor entirely in Boon.**

This document set extends the arena-based engine redesign (Parts 1-12) with FPGA synthesis capabilities (Part 13) and culminates in a full RISC-V softcore implementation (Part 14).

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          BOON LANGUAGE                                       │
│                    Reactive Dataflow Programming                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
              ┌─────────────────────┴─────────────────────┐
              ▼                                           ▼
┌─────────────────────────────┐           ┌─────────────────────────────┐
│   BROWSER / SERVER / CLI    │           │      FPGA / ASIC            │
│   (Arena-Based Engine)      │           │   (HDL Transpiler)          │
│                             │           │                             │
│   Parts 1-12:               │           │   Part 13: Transpiler       │
│   - Arena memory model      │           │   - Boon → SystemVerilog    │
│   - Message-passing         │           │   - Type inference          │
│   - Event loop              │           │   - Elaboration             │
│   - Persistence             │           │                             │
│   - Multi-threading         │           │   Part 14: RISC Softcore    │
│   - Live updates            │           │   - RV32I implementation    │
│   - Testing infrastructure  │           │   - Pipeline stages         │
│                             │           │   - Memory interface        │
└─────────────────────────────┘           └─────────────────────────────┘
```

---

## Document Structure

| Part | Title | File |
|------|-------|------|
| 1-12 | Arena-Based Reactive Dataflow Engine | `10_*` through `22_*` |
| M | First Major Milestone: Playground on New Engine | `01_MIGRATION_MILESTONE.md` |
| 13 | FPGA Circuit Design - Transpiler | `02_FPGA_TRANSPILER.md` |
| 14 | RISC Softcore in Boon | `03_RISC_SOFTCORE.md` |
| 15 | Fixed-Size Philosophy for Browser Runtime | `04_FIXED_SIZE_PHILOSOPHY.md` |

---

## File Index

### High-Level Plans (00-09)
- `00_OVERVIEW.md` - This file
- `01_MIGRATION_MILESTONE.md` - First major milestone: playground on new engine
- `02_FPGA_TRANSPILER.md` - Part 13: Boon → SystemVerilog
- `03_RISC_SOFTCORE.md` - Part 14: RISC-V in Boon
- `04_FIXED_SIZE_PHILOSOPHY.md` - Part 15: Future consideration
- `05_ROADMAP.md` - Complete roadmap summary

### Arena Engine Details (10-22)
- `10_ARENA_ENGINE_OVERVIEW.md` - Executive summary, constraints, architecture
- `11_NODE_IDENTIFICATION.md` - Part 1: SourceId, ScopeId, NodeAddress
- `12_ARENA_MEMORY.md` - Part 2: Arena, SlotId, ReactiveNode
- `13_MESSAGE_PASSING.md` - Part 3: Message, Payload, routing
- `14_EVENT_LOOP.md` - Part 4: EventLoop, timer queue
- `15_GRAPH_SNAPSHOT.md` - Part 5: Serialization, persistence
- `16_FLUSH_ERRORS.md` - Part 6: FLUSH error handling
- `17_MULTI_THREADING.md` - Part 7: WebWorkers, SharedArrayBuffer
- `18_BRIDGE_API.md` - Part 8: Bridge & API architecture
- `19_VIRTUAL_FILESYSTEM.md` - Part 9: Modules, multi-renderer
- `20_LIVE_UPDATES.md` - Part 10: Hot reload, state migration
- `21_CROSS_PLATFORM.md` - Part 11: Browser, server, CLI
- `22_TESTING_INFRASTRUCTURE.md` - Part 12: Native testing

### Reference Materials (23-28)
- `23_MIGRATION_STRATEGY.md` - Implementation phases, migration approach
- `24_KNOWN_ISSUES.md` - Known bugs, corrections, design decisions
- `25_EXAMPLES_MATRIX.md` - Playground examples compatibility matrix
- `26_CRITICAL_FILES.md` - Critical files, risks, success criteria, future directions
- `27_ARCHITECTURE_NOTES.md` - Architecture comparison notes
- `28_DOCUMENTATION_SYNC.md` - Documentation sync needed
