# Boon Runtime Redesign: Arena-Based Reactive Dataflow Engine

## Executive Summary

Redesign Boon's runtime from Arc-heavy async actors to an arena-based, message-passing dataflow engine that:
- Eliminates heap allocations (Arc) for normal operation
- Supports multi-threaded WASM (SharedArrayBuffer + Web Workers)
- Uses hardware-inspired design patterns (not synthesizable, see K3)
- Enables full graph snapshot/restore for persistence

## Design Constraints

| Constraint | Implication |
|------------|-------------|
| Multi-threaded WASM | No RefCell, thread-safe or partitioned arena |
| Hardware-inspired design | Mental model of wires/registers, NOT synthesizable (see K3) |
| Full graph snapshot | All state serializable, no opaque closures |
| Frontend-only (now) | Defer distributed/backend concerns |
| Single-thread default | Multi-threaded WASM is optional (requires COOP/COEP) |

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        Boon Program                              │
│                     (Parsed AST with NodeIds)                    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Static Topology                              │
│              (Nodes + Wires from source code)                    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Runtime Graph                                │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │   Arena 0   │  │   Arena 1   │  │   Arena N   │  (Workers)   │
│  │  (Main UI)  │  │  (Compute)  │  │  (Compute)  │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
│         │                │                │                      │
│         └────────────────┼────────────────┘                      │
│                          ▼                                       │
│              ┌─────────────────────┐                             │
│              │   Message Router    │                             │
│              │   (Cross-Worker)    │                             │
│              └─────────────────────┘                             │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Event Loop                                   │
│  • Timer Queue (priority heap)                                   │
│  • DOM Events (from browser)                                     │
│  • Dirty Node Propagation                                        │
└─────────────────────────────────────────────────────────────────┘
```

---

