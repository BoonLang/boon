# Part 14: RISC Softcore in Boon

**Ultimate Goal:** Implement a complete RISC-V RV32I softcore processor in Boon, synthesizable to FPGA.

---

## 14.1 Why RISC-V in Boon?

1. **Validation:** Proves Boon is a real HDL, not just "hardware-inspired"
2. **Dogfooding:** Uses all transpiler features (FSMs, BITS, LIST, modules)
3. **Showcase:** Demonstrates Boon's readability vs Verilog for complex designs
4. **Practical:** RISC-V is open standard, useful for embedded Boon applications
5. **Research:** Foundation for running Boon on Boon (self-hosting potential)

---

## 14.2 Target Architecture: RV32I

**RV32I Base Integer ISA:**
- 32-bit registers (x0-x31, x0 hardwired to 0)
- 32 base instructions
- Memory-mapped I/O
- Simple enough for clean implementation, complex enough to be useful

**Initial Implementation Goals:**
- [ ] All RV32I instructions
- [ ] 5-stage pipeline (IF, ID, EX, MEM, WB)
- [ ] Hazard detection and forwarding
- [ ] Branch prediction (optional: static not-taken)
- [ ] Memory interface (simple, not cached initially)

---

## 14.3 Processor Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            RISC-V RV32I Core                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐  │
│  │    IF    │──▶│    ID    │──▶│    EX    │──▶│   MEM    │──▶│    WB    │  │
│  │  Fetch   │   │  Decode  │   │ Execute  │   │  Memory  │   │Writeback │  │
│  └──────────┘   └──────────┘   └──────────┘   └──────────┘   └──────────┘  │
│       │              │              │              │              │         │
│       │              ▼              ▼              ▼              │         │
│       │         ┌─────────────────────────────────────┐          │         │
│       │         │           Register File             │◀─────────┘         │
│       │         │            (32 x 32-bit)            │                    │
│       │         └─────────────────────────────────────┘                    │
│       │                          │                                          │
│       ▼                          ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                     Hazard Detection Unit                            │   │
│  │                 (Forwarding, Stalling, Flushing)                     │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
        │                                           │
        ▼                                           ▼
┌───────────────┐                         ┌───────────────────┐
│  Instruction  │                         │   Data Memory     │
│    Memory     │                         │   (RAM / MMIO)    │
└───────────────┘                         └───────────────────┘
```

---

## 14.4 Module Breakdown

### 14.4.1 Top-Level Modules

```boon
-- Top-level RISC-V core
FUNCTION riscv_core(rst, mem_rdata, mem_ready) {
    BLOCK {
        -- Pipeline stages
        if_stage: instruction_fetch(rst: rst, pc: pc, imem_data: mem_rdata)
        id_stage: instruction_decode(rst: rst, instruction: if_stage.instruction, ...)
        ex_stage: execute(rst: rst, alu_op: id_stage.alu_op, ...)
        mem_stage: memory_access(rst: rst, ...)
        wb_stage: writeback(rst: rst, ...)

        -- Register file
        regfile: register_file(
            rst: rst,
            rs1_addr: id_stage.rs1,
            rs2_addr: id_stage.rs2,
            rd_addr: wb_stage.rd,
            rd_data: wb_stage.data,
            rd_we: wb_stage.we
        )

        -- Hazard detection
        hazard: hazard_unit(
            id_rs1: id_stage.rs1,
            id_rs2: id_stage.rs2,
            ex_rd: ex_stage.rd,
            mem_rd: mem_stage.rd,
            ex_mem_read: ex_stage.mem_read,
            ...
        )

        [
            mem_addr: ex_stage.alu_result,
            mem_wdata: ex_stage.rs2_data,
            mem_we: ex_stage.mem_we,
            mem_re: ex_stage.mem_re
        ]
    }
}
```

### 14.4.2 Instruction Fetch Stage

```boon
FUNCTION instruction_fetch(rst, stall, flush, branch_target, branch_taken) {
    BLOCK {
        -- Program counter
        pc: BITS[32] { 10u0 } |> LATEST pc {
            PASSED.clk |> THEN {
                rst |> WHILE {
                    True => BITS[32] { 16u00000000 }  -- Reset vector
                    False => stall |> WHILE {
                        True => pc
                        False => branch_taken |> WHILE {
                            True => branch_target
                            False => pc |> Bits/add(BITS[32] { 10u4 })
                        }
                    }
                }
            }
        }

        [pc: pc, instruction_addr: pc]
    }
}
```

### 14.4.3 Instruction Decode Stage

```boon
-- RV32I instruction formats
InstrType: [R, I, S, B, U, J]

FUNCTION instruction_decode(rst, instruction, regfile_rs1, regfile_rs2) {
    BLOCK {
        -- Extract fields
        opcode: instruction |> Bits/slice(high: 6, low: 0)
        rd:     instruction |> Bits/slice(high: 11, low: 7)
        funct3: instruction |> Bits/slice(high: 14, low: 12)
        rs1:    instruction |> Bits/slice(high: 19, low: 15)
        rs2:    instruction |> Bits/slice(high: 24, low: 20)
        funct7: instruction |> Bits/slice(high: 31, low: 25)

        -- Immediate extraction (format-dependent)
        imm_i: instruction |> Bits/slice(high: 31, low: 20) |> Bits/sign_extend(to: 32)
        imm_s: [
            instruction |> Bits/slice(high: 31, low: 25),
            instruction |> Bits/slice(high: 11, low: 7)
        ] |> Bits/concat() |> Bits/sign_extend(to: 32)
        imm_b: [
            instruction |> Bits/get(index: 31),
            instruction |> Bits/get(index: 7),
            instruction |> Bits/slice(high: 30, low: 25),
            instruction |> Bits/slice(high: 11, low: 8),
            BITS[1] { 10u0 }
        ] |> Bits/concat() |> Bits/sign_extend(to: 32)
        imm_u: instruction |> Bits/slice(high: 31, low: 12) |> Bits/concat(BITS[12] { 10u0 })
        imm_j: [
            instruction |> Bits/get(index: 31),
            instruction |> Bits/slice(high: 19, low: 12),
            instruction |> Bits/get(index: 20),
            instruction |> Bits/slice(high: 30, low: 21),
            BITS[1] { 10u0 }
        ] |> Bits/concat() |> Bits/sign_extend(to: 32)

        -- Decode instruction type
        instr_type: opcode |> WHEN {
            BITS[7] { 2b0110011 } => R   -- R-type (ADD, SUB, etc.)
            BITS[7] { 2b0010011 } => I   -- I-type immediate (ADDI, etc.)
            BITS[7] { 2b0000011 } => I   -- Load
            BITS[7] { 2b0100011 } => S   -- Store
            BITS[7] { 2b1100011 } => B   -- Branch
            BITS[7] { 2b0110111 } => U   -- LUI
            BITS[7] { 2b0010111 } => U   -- AUIPC
            BITS[7] { 2b1101111 } => J   -- JAL
            BITS[7] { 2b1100111 } => I   -- JALR
            __ => I  -- Default
        }

        -- Select immediate based on type
        immediate: instr_type |> WHEN {
            I => imm_i
            S => imm_s
            B => imm_b
            U => imm_u
            J => imm_j
            __ => BITS[32] { 10u0 }
        }

        -- Decode ALU operation
        alu_op: decode_alu_op(opcode: opcode, funct3: funct3, funct7: funct7)

        [
            rs1: rs1,
            rs2: rs2,
            rd: rd,
            rs1_data: regfile_rs1,
            rs2_data: regfile_rs2,
            immediate: immediate,
            alu_op: alu_op,
            instr_type: instr_type
        ]
    }
}
```

### 14.4.4 ALU

```boon
AluOp: [Add, Sub, Sll, Slt, Sltu, Xor, Srl, Sra, Or, And, Eq, Ne, Lt, Ge, Ltu, Geu]

FUNCTION alu(op, a, b) {
    op |> WHEN {
        Add  => a |> Bits/add(b)
        Sub  => a |> Bits/subtract(b)
        Sll  => a |> Bits/shift_left(by: b |> Bits/slice(high: 4, low: 0))
        Slt  => a |> Bits/signed_less_than(b) |> Bool/to_bits(width: 32)
        Sltu => a |> Bits/less_than(b) |> Bool/to_bits(width: 32)
        Xor  => a |> Bits/xor(b)
        Srl  => a |> Bits/shift_right(by: b |> Bits/slice(high: 4, low: 0))
        Sra  => a |> Bits/shift_right_arith(by: b |> Bits/slice(high: 4, low: 0))
        Or   => a |> Bits/or(b)
        And  => a |> Bits/and(b)
        Eq   => a |> Bits/equal(b) |> Bool/to_bits(width: 32)
        Ne   => a |> Bits/not_equal(b) |> Bool/to_bits(width: 32)
        Lt   => a |> Bits/signed_less_than(b) |> Bool/to_bits(width: 32)
        Ge   => a |> Bits/signed_greater_equal(b) |> Bool/to_bits(width: 32)
        Ltu  => a |> Bits/less_than(b) |> Bool/to_bits(width: 32)
        Geu  => a |> Bits/greater_equal(b) |> Bool/to_bits(width: 32)
    }
}
```

### 14.4.5 Register File

```boon
FUNCTION register_file(rst, rs1_addr, rs2_addr, rd_addr, rd_data, rd_we) {
    BLOCK {
        -- 32 registers, x0 hardwired to 0
        regs: LIST[32] { BITS[32] { 10u0 } } |> LATEST regs {
            PASSED.clk |> THEN {
                rst |> WHILE {
                    True => LIST[32] { BITS[32] { 10u0 } }
                    False => rd_we |> WHILE {
                        False => regs
                        True => rd_addr |> Bits/equal(BITS[5] { 10u0 }) |> WHILE {
                            True => regs  -- x0 stays 0
                            False => regs |> List/set(index: rd_addr, value: rd_data)
                        }
                    }
                }
            }
        }

        -- Read ports (combinational)
        rs1_data: rs1_addr |> Bits/equal(BITS[5] { 10u0 }) |> WHILE {
            True => BITS[32] { 10u0 }
            False => regs |> List/get(index: rs1_addr)
        }

        rs2_data: rs2_addr |> Bits/equal(BITS[5] { 10u0 }) |> WHILE {
            True => BITS[32] { 10u0 }
            False => regs |> List/get(index: rs2_addr)
        }

        [rs1_data: rs1_data, rs2_data: rs2_data]
    }
}
```

---

## 14.5 Implementation Phases

### Phase 14A: Single-Cycle RV32I

**Goal:** Non-pipelined processor executing one instruction per cycle

1. Implement instruction fetch (PC + memory interface)
2. Implement decoder for all RV32I formats
3. Implement ALU with all operations
4. Implement register file
5. Implement memory interface (load/store)
6. Test with simple assembly programs

### Phase 14B: 5-Stage Pipeline

**Goal:** Pipelined execution with hazard handling

1. Add pipeline registers between stages
2. Implement data forwarding (EX→EX, MEM→EX)
3. Implement hazard detection (load-use stall)
4. Implement branch handling (flush on taken)
5. Test with more complex programs

### Phase 14C: Memory System

**Goal:** Practical memory interface

1. Implement instruction cache (direct-mapped, small)
2. Implement data cache (optional)
3. Add memory-mapped I/O support
4. Integrate with UART (from super_counter!)

### Phase 14D: Verification

**Goal:** Ensure correctness

1. Run RISC-V compliance tests
2. Run simple C programs (compiled with gcc)
3. Compare against Spike or other reference
4. Performance benchmarks

---

## 14.6 Boon Language Features Used

| Feature | RISC-V Usage |
|---------|--------------|
| `BITS[N]` | All data paths (32-bit), addresses, immediates |
| `LIST[32, BITS[32]]` | Register file |
| `Tags` | Instruction types, ALU operations, pipeline stages |
| `WHEN` | Opcode decoding, ALU operation selection |
| `WHILE` | Control signal multiplexing, hazard handling |
| `HOLD/LATEST` | Pipeline registers, PC, register file state |
| `FUNCTION` | Each pipeline stage, ALU, decoder, etc. |
| `BLOCK` | Local signal scoping |
| `PASSED.clk` | Clock distribution |
| `Bits/slice` | Field extraction from instructions |
| `Bits/concat` | Immediate reconstruction |
| `Bits/sign_extend` | Sign extension for immediates |
| `List/get` | Register file read |
| `List/set` | Register file write |

---

## 14.7 Success Criteria

### Phase 14A (Single-Cycle)
- [ ] All 37 RV32I instructions execute correctly
- [ ] Simple "hello world" assembly runs
- [ ] Synthesizes for ICE40 at reasonable size

### Phase 14B (Pipeline)
- [ ] Pipeline achieves higher clock frequency
- [ ] Hazard handling verified with test programs
- [ ] Branch prediction working (static not-taken)

### Phase 14C (Memory)
- [ ] Can run C programs from external memory
- [ ] UART output working (reuse super_counter modules!)
- [ ] Memory-mapped LED/button I/O

### Phase 14D (Complete)
- [ ] Passes RISC-V compliance test suite
- [ ] Documented and clean Boon code
- [ ] Performance comparable to similar Verilog implementations

### Phase 14E: Self-Hosting - Boon on Boon RISC (ULTIMATE VISION)
- [ ] Compile Boon runtime to RISC-V target (via Rust → RISC-V cross-compile)
- [ ] Run Boon WASM runtime on the Boon-designed RISC-V core
- [ ] Execute `.bn` programs on hardware designed in `.bn`
- [ ] **Blog post**: "Running Boon on a RISC-V Designed in Boon"

**The Self-Hosting Stack:**
```
┌─────────────────────────────────────────────┐
│         Boon Programs (.bn)                 │
│         (user applications)                 │
├─────────────────────────────────────────────┤
│         Boon Runtime (WASM → RV32)          │
│         (arena-based engine, Parts 1-12)   │
├─────────────────────────────────────────────┤
│         RISC-V RV32I Core                   │
│         (written in Boon, Part 14)          │
├─────────────────────────────────────────────┤
│         FPGA Fabric (ICE40/ECP5)            │
│         (synthesized via Part 13)           │
└─────────────────────────────────────────────┘
```

This completes the circle: **Boon designs the hardware that runs Boon.**

---

## 14.8 Critical Files

| File | Purpose |
|------|---------|
| `riscv/core.bn` | **NEW:** Top-level processor |
| `riscv/fetch.bn` | **NEW:** Instruction fetch stage |
| `riscv/decode.bn` | **NEW:** Instruction decode stage |
| `riscv/execute.bn` | **NEW:** Execute stage + ALU |
| `riscv/memory.bn` | **NEW:** Memory access stage |
| `riscv/writeback.bn` | **NEW:** Writeback stage |
| `riscv/regfile.bn` | **NEW:** Register file |
| `riscv/hazard.bn` | **NEW:** Hazard detection unit |
| `riscv/tests/` | **NEW:** Assembly test programs |

---

