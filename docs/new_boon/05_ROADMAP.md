## Summary: Complete Roadmap

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        BOON COMPLETE REDESIGN ROADMAP                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Parts 1-12: Arena-Based Engine (agile-forging-locket.md)                  │
│  ├── Part 1-4: Core architecture (IDs, arena, messages, event loop)        │
│  ├── Part 5-7: Persistence, error handling, multi-threading                │
│  ├── Part 8-10: Bridge/API, modules, live updates                          │
│  └── Part 11-12: Cross-platform, testing infrastructure                    │
│                                                                             │
│  ══════════════════════════════════════════════════════════════════════    │
│  FIRST MAJOR MILESTONE: Playground on New Engine (~12 weeks)               │
│  ├── Phase 1-2: Core types, arena, messages                                │
│  ├── Phase 3-4: Basic nodes, combinators (counter.bn works)                │
│  ├── Phase 5-6: Lists, timers (interval.bn works)                          │
│  └── Phase 7: Bridge & UI (todo_mvc.bn works) ◀── MILESTONE COMPLETE       │
│  ══════════════════════════════════════════════════════════════════════    │
│                                                                             │
│  Part 13: FPGA Transpiler (this document)                                  │
│  ├── Phase 13A: Minimal HIR + CodeGen (FSM)                                │
│  ├── Phase 13B: BITS type system                                           │
│  ├── Phase 13C: Fixed-size lists                                           │
│  ├── Phase 13D: super_counter milestone ◀── FPGA MILESTONE                 │
│  └── Phase 13E: CLI + tooling                                              │
│                                                                             │
│  Part 14: RISC Softcore (this document)                                    │
│  ├── Phase 14A: Single-cycle RV32I                                         │
│  ├── Phase 14B: 5-stage pipeline                                           │
│  ├── Phase 14C: Memory system                                              │
│  ├── Phase 14D: Verification                                               │
│  └── Phase 14E: Self-hosting ◀── ULTIMATE GOAL: BOON ON BOON RISC          │
│                                                                             │
│  Part 15: Fixed-Size Philosophy (future consideration)                     │
│  └── Unify browser/FPGA with fixed-size core reactive state                │
│                                                                             │
│  ═══════════════════════════════════════════════════════════════════════   │
│                                                                             │
│  MILESTONE PROGRESSION:                                                     │
│                                                                             │
│  todo_mvc.bn on new engine        "Boon runtime is production-ready"       │
│         ↓                                                                   │
│  super_counter.bn → working SV    "Boon can make hardware"                 │
│         ↓                                                                   │
│  RISC-V passes compliance         "Boon can make a processor"              │
│         ↓                                                                   │
│  Boon runs on Boon RISC           "Boon designs its own substrate"         │
│         ↓                                                                   │
│  📝 BLOG POST: "Running Boon on a RISC-V Designed in Boon"                 │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

