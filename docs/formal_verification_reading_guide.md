# Formal Verification Reading Guide for Boon

**Priority:** Boon language design — proofs as part of the language, AI code generation + verification.

## Two Distinct Goals

This guide serves two related but distinct goals. Knowing which goal each resource serves helps you understand WHY you're reading it.

**Goal A: Verification IN Boon** — Giving Boon users and AI the ability to prove their programs correct. This is a *language feature*: Boon's constructs, type system, and compiler work together so that correctness properties are either inferred automatically or stated as lightweight annotations. This is the primary goal and drives the reading order. Most phases serve Goal A; each resource is tagged in the summary table.

**Goal B: Verification OF Boon** — Proving that Boon's own implementation (Actors engine, DD engine, WASM engine, parser, evaluator) is correct. This uses external tools (K Framework, TLA+, Lean/Rocq) to verify that Boon itself does what it claims. Important, but secondary to the language design. Some resources (especially Phases 6 and 8) primarily serve Goal B; some serve both.

Most resources serve Goal A. When a resource primarily serves Goal B, it's noted explicitly.

---

## The Design Landscape for "Proofs in Boon"

There are roughly 4 levels of how languages embed verification, from lightest to deepest:

### Level 1: Contracts (Dafny, SPARK, Verus)
```
// Dafny-style: pre/post conditions + loop invariants
method Abs(x: int) returns (y: int)
  ensures y >= 0
  ensures y == x || y == -x
{
  if x < 0 then y := -x else y := x;
}
```
The compiler generates **verification conditions** and discharges them automatically with an SMT solver (Z3). The programmer writes annotations, the machine does the proving. **This is the most AI-friendly approach** — contracts are just annotations that an LLM can generate alongside code.

### Level 2: Refinement Types (Liquid Haskell, F*)
```
// Types carry logical predicates
type Positive = { x: Int | x > 0 }
// Compiler automatically checks that all values
// flowing into a Positive slot satisfy x > 0
```
Lighter than full contracts — the type system does most of the work. Very natural for dataflow languages like Boon where values flow through typed streams.

### Level 3: Dependent Types (Lean 4, Idris 2)
```
// Types can depend on values — proofs are values
def Vector (n : Nat) (α : Type) : Type  -- length-indexed vector
-- The type ITSELF guarantees length invariants
```
Most powerful, but proofs can be complex. Lean 4 is both a programming language and a proof assistant — the line between code and proof disappears.

### Level 4: Full Formal Semantics (K Framework)
You define the language's meaning mathematically, then reasoning about programs follows from the semantics. Most foundational, but users don't write proofs directly.

### Cross-Cutting Dimension: Temporal Properties (LTL/CTL, Model Checking)

The levels above describe *what kind* of proofs you can write. But there's an orthogonal dimension: *what kind of properties* you want to verify. For reactive dataflow languages like Boon, the most natural properties are **temporal**:

- **Safety:** "The counter is *always* >= 0." (Invariant over time)
- **Liveness:** "If a button is pressed, the counter *eventually* updates." (Progress guarantee)
- **Reactivity:** "Whenever input changes, output *next* reflects it." (Response guarantee)
- **Until:** "The loading spinner shows *until* data arrives." (Bounded waiting)

These aren't naturally expressible as Hoare-logic contracts (pre/postconditions). They require **temporal logic** (LTL, CTL) and **model checking** (BMC, k-induction, IC3) — the same techniques from the ZipCPU blog post, but applied to software streams instead of hardware signals.

This matters for Boon because its constructs are inherently temporal: `HOLD` is state over time, `THEN` is "when event arrives," `WHILE` is "during condition," `LATEST` is "whenever any input changes." The Lustre language (Phase 1c) and Kind 2 model checker (Phase 5b) show how temporal verification works for dataflow programs. Boon's built-in verifier will likely need both Hoare-logic contracts (for value properties) AND temporal model checking (for behavior-over-time properties).

## What This Means for Boon Specifically

Boon's **reactive dataflow nature is a massive advantage** for verification. Here's why:

Imperative code is hard to verify because you need loop invariants and reasoning about mutable state at every step. But Boon's constructs have algebraic structure:

```boon
// A HOLD is essentially a fold with a state invariant
// What would verified Boon look like?
[count: 0] |> HOLD state
  ENSURES state.count >= 0    // ← state invariant
{
  event |> THEN { [count: state.count + 1] }
}

// A stream could carry type-level guarantees
temperatures: sensor |> WHEN {
  value WHERE value > -273.15 => value   // ← physical law as a filter
}
// The type system could PROVE that downstream consumers
// never see physically impossible temperatures
```

**Contracts (Level 1) fit Boon naturally:**
- `HOLD` → state invariant (holds before and after each update)
- `THEN`/`WHEN`/`WHILE` → preconditions on pattern matches, postconditions on outputs
- Streams → refinement types on values flowing through ("this stream only carries positive integers")
- `LATEST` → relationship invariants between combined streams

**For AI generation, this means:**
1. AI generates Boon code with contracts/invariants
2. Boon compiler verifies contracts automatically (via SMT solver like Z3)
3. If proofs fail, AI gets feedback and iterates
4. If proofs pass, the code is **mathematically guaranteed correct**

The feedback loop is key: the compiler's proof failures are structured error messages that AI can act on — much better than "it crashed at runtime."

**The cost problem:** Traditional verification is expensive. CompCert (a verified C compiler) is 42,000 lines of Coq — **76% of which is proof code**, with a 3:1 proof-to-program ratio and ~3 person-years of effort. This is why formal verification hasn't gone mainstream: the cost of writing proofs dwarfs the cost of writing code.

**Boon's answer:** The intrinsic verification thesis (detailed at the end of this document) aims for a fundamentally different ratio. If the language constructs themselves encode verification properties, Layer 1 verification is essentially **free** — 0:1 proof-to-code ratio for many common properties. This changes the economics entirely, especially for AI generation where the AI doesn't need to generate separate proofs at all.

---

## Critical Path: Reading Order

This is the **essential** reading list, in order. Each phase builds on the previous. Supplementary materials for each topic are listed in the section after this.

### Phase 0: Motivation — "Why This Matters Now" (Day 1)

**0a. [Martin Kleppmann: "AI will make formal verification go mainstream"](https://martin.kleppmann.com/2025/12/08/ai-formal-verification.html)** — Blog post, 15 min

Read this first. Kleppmann argues that (1) LLMs are becoming skilled at writing proof scripts, (2) AI-generated code *creates the need* for formal verification, and (3) the precision of formal verification counteracts the probabilistic nature of LLMs. His vision — AI generates code + proofs, compiler checks proofs — is exactly the Boon vision. Mentions CompCert, seL4, Project Everest as existing successes.

**0b. [ZipCPU: A Beginner's Guide to Formal Verification](https://zipcpu.com/blog/2017/10/19/formal-intro.html)** — Blog post, 30 min

Concrete motivation: a hardware designer applies formal verification (BMC, induction) to a FIFO and finds bugs that testing missed. Shows that formal methods are practical, not just theoretical. Also relevant to Boon's future FPGA target.

---

### Phase 1: First Taste — Zero-Commitment Proof Experience (Days 2-3)

**1a. [Natural Number Game (Lean 4)](https://adam.math.hhu.de/#/g/leanprover-community/NNG4)** — Browser game, 2-3 hours

Gamified introduction to proofs. You prove 2+2=4, then commutativity of addition, then build up number theory — all in the browser, no installation. This gives you the "feel" of constructing proofs before committing to a textbook.

**1b. [AdaCore: Introduction to Formal Verification with SPARK (Video)](https://www.adacore.com/videos/introduction-to-formal-verification-with-spark)** — Webinar, ~1 hour

Visual introduction to what "proofs in a production language" looks like. Real-world examples. Watch this before reading the SPARK book — it gives you the big picture.

**1c. [The Synchronous Dataflow Programming Language LUSTRE (PDF)](https://homepage.cs.uiowa.edu/~tinelli/classes/181/Spring08/Papers/Halb91.pdf)** — Foundational paper, 1 day

**Read this before diving into Dafny.** Lustre is the closest existing language to Boon's model — programs are compositions of infinite streams with temporal operators. The key insight that reframes everything you'll read afterward: **programs and their verification properties are written in the same language.** Lustre's temporal operators map to Boon's HOLD, LATEST, WHEN, WHILE. When you read Dafny next, you'll be thinking "how does this apply to dataflow?" instead of trying to translate imperative concepts later.

This paper grounds the entire reading journey in Boon's actual paradigm: reactive dataflow, not imperative procedures.

---

### Phase 2: Core Theory — "What Are Proofs in a Language?" (Weeks 1-6)

Now that the Lustre paper has grounded you in dataflow verification, you need the vocabulary of verification itself — contracts, invariants, termination, induction. Dafny teaches this best, even though its examples are imperative. As you read, keep asking: "how would this concept look in a dataflow language like Boon?"

**2a. [Program Proofs — K. Rustan M. Leino (MIT Press)](https://mitpress.mit.edu/9780262546232/program-proofs/)** — Book, 3-4 weeks

**THE textbook. This is the single most important resource.** Leino invented Dafny to answer "what does it look like when proofs are part of a programming language?" Three parts: foundations (termination, induction), functional programs, imperative programs (with objects and dynamic frames). All in runnable Dafny code, not pseudocode.

After this book, you'll know: ensures, requires, invariant, decreases, lemmas, ghost variables, induction, termination proofs. Everything else builds on this vocabulary.

**Design takeaway for Boon:** Dafny's annotation style (contracts as lightweight decorators) is the most AI-compatible approach. It's the model to start from.

Companion: [Dafny Getting Started Guide (CMU PDF)](https://www.andrew.cmu.edu/course/18-330/2025s/reading/dafny_guide.pdf) — Quick reference alongside the book.

**2b. [Building High Integrity Applications with SPARK](https://www.amazon.com/Building-High-Integrity-Applications-SPARK/dp/1107656842)** — Book, 1-2 weeks

Same ideas as Dafny but in a production language used in avionics, medical devices, and railway signaling. The critical design lesson is **graduated verification** — SPARK's Stone → Bronze → Silver → Gold → Platinum levels let teams choose how much to verify. Also covers information flow analysis (proving data only flows where it should), which maps directly to Boon's dataflow model.

**Design takeaway for Boon:** Graduated verification layers. Boon could offer: "Layer 0: runtime stream assertions" (monitor values), "Layer 1: no runtime errors + stream type invariants" (automatic inference), "Layer 2: full functional correctness" (explicit static assertions).

Companion: [AdaCore learn.adacore.com SPARK Course](https://learn.adacore.com/courses/intro-to-spark/chapters/01_Overview.html) — Free, interactive, browser-based SPARK exercises.

---

### Phase 3: AI + Proof Generation — "Machines Writing Proofs" (Week 7)

Now that you understand proofs from the inside, see how AI does it.

**3a. [AutoVerus: Automated Proof Generation for Rust Code](https://arxiv.org/abs/2409.13082)** — Paper, OOPSLA 2025

Microsoft Research. LLM agents automatically generate Verus proofs for Rust code. Three-phase approach mimicking human experts: preliminary generation, refinement with tips, debugging with verification errors. **90%+ success rate** on 150 non-trivial tasks, most in <30 seconds.

**Why read this:** This is the closest existing system to what Boon's AI proof pipeline would look like. The three-phase architecture (generate → refine → debug) is the blueprint.

**3b. [DafnyBench: A Benchmark for Formal Software Verification](https://namin.seas.harvard.edu/pubs/dafnybench.pdf)** — Paper, POPL 2025

750+ Dafny programs, 53K lines. Tests LLMs on filling proof annotations (remove all assert/invariant, ask LLM to fill them back). Shows the feedback loop: generate → verify → retry with error messages. Performance improved from 68% to 96% over one year.

**Why read this:** Shows the benchmark methodology for measuring AI proof capabilities. When Boon has built-in proofs, you'd build a similar benchmark ("BoonBench").

---

### Phase 4: The Verification Design Space — "What to Embed in Boon" (Weeks 8-11)

Now you know contracts (Dafny/SPARK) and how AI uses them. Explore the wider design space to make informed choices for Boon.

**4a. [Liquid Haskell Tutorial (PDF)](https://ucsd-progsys.github.io/liquidhaskell-tutorial/book.pdf)** — Free book/tutorial, ~1 week

Refinement types for Haskell. Types carry predicates (e.g., `{ x: Int | x > 0 }`), checked via SMT. 10,000+ lines of Haskell libraries verified. **The lightest-weight verification approach** — most natural for Boon's streams where values flow through typed pipelines.

**Design takeaway for Boon:** Refinement types on streams could be the core of Boon's Layer 1 verification — requiring minimal annotations while catching many real bugs. E.g., `temperatures: Stream { t: Number | t > -273.15 }`.

**4b. F\*: key insight (read the tutorial only if time permits)**

[F*](https://fstar-lang.org/) combines ALL three verification levels (contracts, refinement types, dependent types) with **monadic effects** and SMT automation. Built Project Everest (verified HTTPS stack). The full [F* tutorial](https://fstar-lang.org/tutorial/) is 1-2 weeks and may be overwhelming at this stage — but the key design insight is essential:

**F* shows how to verify effectful code.** It uses monadic effect types to describe what a computation *does* (reads state, might fail, performs I/O) alongside what it *returns*. Boon's streams and reactivity are effects — a stream that emits values over time is fundamentally an effectful computation. F*'s approach could inform how Boon types and verifies reactive computations: a `HOLD` has a "state effect," a `THEN` has a "trigger effect," an external API call has an "I/O effect."

If you have time, the full tutorial is in the supplementary section. If not, this paragraph captures the design takeaway.

**4c. [Programming Z3 (Stanford)](https://theory.stanford.edu/~nikolaj/programmingz3.html)** — Online guide, 1-2 weeks

By Z3's creator, Nikolaj Bjorner. Z3 is the SMT solver that powers Dafny, Verus, F*, SPARK, and Liquid Haskell. If Boon embeds verification, it will use Z3 (or a similar SMT solver) as the proof engine. This guide teaches you what Z3 can and can't do — essential for understanding what proofs are "easy" (decidable theories) vs. "hard" (need human hints).

**Design takeaway for Boon:** Understanding Z3's capabilities shapes what Boon's proof annotations look like. Properties in decidable theories (linear arithmetic, arrays, bitvectors) verify automatically; others need hints.

Companion: [Z3 Online Guide (Microsoft)](https://microsoft.github.io/z3guide/docs/logic/intro/) — Interactive Z3 tutorial in the browser.

**4d. [Verus: Verified Systems Programming in Rust](https://verus-lang.github.io/event-sites/2024-sosp/)** — Tutorial (SOSP 2024), 1-2 days

SMT-based verification for Rust. Proofs written in Rust syntax using "ghost code" that compiles away. Two OSDI 2024 best papers built on it. Industrial use at Microsoft and Amazon.

**Design takeaway for Boon:** Verus shows how to add verification to an existing language with minimal syntactic disruption. Its "ghost code" concept — proof-only code that's erased at compile time — is a pattern Boon could adopt.

---

### Phase 5: Verified Dataflow Tools — "Building Boon's Verifier" (Weeks 12-13)

Phase 4 taught you the design space (refinement types, SMT solvers, ghost code). Now bring that knowledge back to Boon's actual paradigm. You read the Lustre paper in Phase 1c — now dive deeper into the tools that verify dataflow programs. These are the direct templates for Boon's built-in verifier.

**5a. [Vélus: Verified Lustre Compiler](https://velus.inria.fr/)** — Tool + papers, 1 day

A formally verified compiler from Lustre to assembly, built on CompCert and verified in Coq. End-to-end proof: dataflow semantics of source → traces of generated assembly. **This is what a verified Boon compiler would look like** (Goal B). But it's also relevant to Goal A: Vélus's semantic model of dataflow programs shows what Boon's internal verification engine needs to reason about.

**5b. [Kind 2 Model Checker](https://kind2-mc.github.io/kind2/)** — Tool, 1 day

Multi-engine SMT-based model checker for Lustre programs. Supports assume-guarantee contracts, BMC, k-induction, IC3. Used industrially (Collins Aerospace). Takes Lustre + property annotations, proves them or finds counterexamples.

**Design takeaway for Boon:** Kind 2's contract syntax and temporal property verification for dataflow programs is the most direct template for Boon's built-in verifier. Its assume-guarantee contracts on stream nodes map to contracts on Boon functions. Its temporal property checking (safety, liveness) maps to properties about Boon's HOLD/WHEN/WHILE behavior over time.

**5c. [Formal Verification for Event Stream Processing (BeepBeep)](https://www.sciencedirect.com/science/article/pii/S0890540123000615)** — Paper, 1 day

Shows how to export stream processing pipelines as Kripke structures for model checking with NuXmv. Relevant technique for verifying Boon's stream pipelines — an alternative to Kind 2's approach.

---

### Phase 6: Define Boon's Meaning (Weeks 14-16)

**6. [K Framework](https://kframework.org/)** — Framework, 2-3 weeks

Define Boon's semantics formally using rewrite rules. Get a reference interpreter, model checker, and deductive verifier for free. K's cell-based configuration model maps well to Boon's actor cells. Used to formally define C, Java, JavaScript, Solidity, EVM.

**Design takeaway for Boon:** K could be Boon's "semantic backbone" — define the language once, prove the semantics correct, then verify that each engine (Actors, DD, WASM) matches the K spec.

---

### Phase 7: Deep Proof Theory — "The Maximalist End" (Weeks 17-22)

These teach you the most powerful proof techniques. Not strictly necessary for Boon's initial design, but inform the ceiling of what's possible.

**7a. [Hitchhiker's Guide to Lean 4](https://lean-forward.github.io/hitchhikers-guide/2023/)** — Textbook, 3-4 weeks

Interactive theorem proving. Dependent types, tactics, the Curry-Howard correspondence. Shows the ceiling — what's possible when proofs are first-class. Lean 4 is both a programming language and a proof assistant.

**Design takeaway for Boon:** Selectively borrow ideas (e.g., Lean's `simp` tactic for automatic simplification, or `decide` for decidable propositions). You probably don't want full dependent types in Boon.

**7b. [Type-Driven Development with Idris — Edwin Brady (Manning)](https://www.manning.com/books/type-driven-development-with-idris)** — Book, 2-3 weeks

By Idris's creator. Practical dependent types: I/O, concurrency, state machines, interactive programs. **More practical than Lean's math focus** — better for a language designer thinking about how dependent types work in real programs.

**Design takeaway for Boon:** Idris's state machine verification is directly relevant — Boon's HOLD + WHEN/WHILE patterns are essentially state machines. Idris shows how to prove state machine properties with types.

**7c. [Rocq/Coq — Software Foundations series](https://rocq-prover.org/docs#beginner_section)** — Books (free online), 3-4 weeks

Volume 1: Logical Foundations. Volume 3: Verified Functional Algorithms. Teaches how to **prove data structure and algorithm correctness** — what you'd need for a verified Boon standard library.

**Design takeaway for Boon:** A verified standard library is a force multiplier. AI-generated code using verified primitives needs fewer proofs of its own.

---

### Phase 8: System Specification — "Engine & Concurrency Verification" (As Needed)

Less central to Boon's *language* design, but valuable for verifying Boon's *implementation*.

**8a. [Learn TLA+ — Hillel Wayne (free online)](https://learntla.com/)** — Free book

More accessible than Lamport's book. Practical, by a working consultant. Covers PlusCal. Good for specifying Boon's actor engine concurrency.

**8b. [Specifying Systems: TLA+ — Leslie Lamport (free PDF)](https://lamport.azurewebsites.net/tla/book-02-08-08.pdf)** — Book

The comprehensive TLA+ reference. For specifying and model-checking Boon's actor message passing, channel semantics, reactive update propagation.

**8c. [Formal Software Design with Alloy 6 (free online)](https://haslab.github.io/formal-software-design/overview/index.html)** — Free online book

Lightweight formal methods — model your design, find bugs via SAT solving. No proofs needed, just models. Could inspire a lightweight "design modeling" mode for Boon.

Classic reference: [Software Abstractions — Daniel Jackson](https://www.amazon.com/Software-Abstractions-Logic-Language-Analysis/dp/0262017156)

---

### Phase 9: Hardware Verification (When FPGA Target Active)

**9a. [Introduction to Formal Hardware Verification (Springer)](https://link.springer.com/book/10.1007/978-3-662-03809-3)** — Textbook

Model checking, symbolic simulation, theorem proving for digital circuits.

**9b. [Verification of Reactive Systems — Klaus Schneider (Springer)](https://www.amazon.com/Verification-Reactive-Systems-Algorithms-Theoretical/dp/3642055559)** — Textbook

Comprehensive: mu-calculus, omega-automata, temporal logics for reactive systems. Academic but directly applicable to Boon's reactive model and hardware targets.

---

## Supplementary Resources by Topic

These complement the critical path. Read them when you reach the relevant phase, or as reference.

### AI + Formal Verification Frontier

| Resource | What | Why |
|----------|------|-----|
| [VERINA: Benchmarking Verifiable Code Generation](https://arxiv.org/pdf/2505.23135) | 189 challenges: natural language → verified code (2025) | End-to-end benchmark methodology for Boon |
| [Propose, Solve, Verify (PSV)](https://arxiv.org/html/2512.18160v1) | Self-play: AI proposes problems, solves, verifies (2025) | Self-improving AI verification — future Boon tooling |
| [Apple Hilbert](https://machinelearning.apple.com/research/hilbert) | Recursively builds formal proofs with informal reasoning (2025) | Informal→formal proof bridge pattern |
| [Dafny 2025 Workshop (POPL)](https://popl25.sigplan.org/home/dafny-2025) | Papers: "Dafny as Verification-Aware IR", "Lean on Dafny" | Dafny as compilation target; connecting SMT ↔ interactive proofs |
| [Towards Formal Verification of LLM-Generated Code](https://arxiv.org/abs/2507.13290) | Formal verification pipeline for AI-generated code (2025) | Survey of the field |

### Dafny Ecosystem

| Resource | What |
|----------|------|
| [Dafny Official Docs](https://dafny.org/dafny/DafnyRef/DafnyRef) | Language reference |
| [Dafny Getting Started Guide (CMU)](https://www.andrew.cmu.edu/course/18-330/2025s/reading/dafny_guide.pdf) | Concise tutorial by Koenig & Leino |
| [Dafny on GitHub](https://github.com/dafny-lang/dafny) | Source, examples, issues |

### SPARK Ecosystem

| Resource | What |
|----------|------|
| [SPARK Interactive Course](https://learn.adacore.com/courses/intro-to-spark/chapters/01_Overview.html) | Free browser-based exercises |
| [SPARK Webinar Video](https://www.adacore.com/videos/introduction-to-formal-verification-with-spark) | Visual introduction with real-world examples |
| [SPARK User's Guide Tutorial](https://docs.adacore.com/spark2014-docs/html/ug/en/tutorial.html) | Official hands-on tutorial |
| [SPARK Tutorial Slides (IEEE SecDev)](https://secdev.ieee.org/wp-content/uploads/2019/09/SPARK-Tutorial-Slides.pdf) | Workshop slides |
| [AdaCore Blog: Formal Verification Made Easy](https://blog.adacore.com/formal-verification-made-easy) | Accessible intro post |

### SMT Solvers (Z3)

| Resource | What |
|----------|------|
| [Programming Z3 (Stanford)](https://theory.stanford.edu/~nikolaj/programmingz3.html) | By Z3's creator — how to program with it |
| [Z3 Online Guide (Microsoft)](https://microsoft.github.io/z3guide/docs/logic/intro/) | Interactive browser tutorial |
| [Z3 Jupyter Tutorial](https://github.com/philzook58/z3_tutorial) | Python notebook with examples |
| [Z3 on GitHub](https://github.com/Z3Prover/z3) | Source, docs, bindings |

### Refinement Types & F*

| Resource | What |
|----------|------|
| [Liquid Haskell Tutorial (PDF)](https://ucsd-progsys.github.io/liquidhaskell-tutorial/book.pdf) | Full refinement types tutorial |
| [Why Liquid Haskell Matters (Tweag)](https://www.tweag.io/blog/2022-01-19-why-liquid-haskell/) | Motivation and overview |
| [F* Tutorial](https://fstar-lang.org/tutorial/) | Full proof-oriented programming tutorial |
| [F* POPL 2016 Paper](https://dl.acm.org/doi/10.1145/2837614.2837655) | Canonical reference: dependent types + monadic effects |

### Verus (Verified Rust)

| Resource | What |
|----------|------|
| [Verus SOSP 2024 Tutorial](https://verus-lang.github.io/event-sites/2024-sosp/) | Hands-on tutorial |
| [Verus Paper (extended)](https://arxiv.org/abs/2303.05491) | Linear ghost types for verified Rust |
| [AutoVerus Paper](https://arxiv.org/abs/2409.13082) | AI-automated Verus proofs |
| [Asterinas: Verified OS in Rust](https://asterinas.github.io/2025/02/13/towards-practical-formal-verification-for-a-general-purpose-os-in-rust.html) | Real-world Verus application |

### Lean 4 Ecosystem

| Resource | What |
|----------|------|
| [Natural Number Game](https://adam.math.hhu.de/#/g/leanprover-community/NNG4) | Gamified browser tutorial |
| [Lean Game Server](https://adam.math.hhu.de/) | More games: Set Theory, Logic, Robo |
| [Mathematics in Lean](https://leanprover-community.github.io/mathematics_in_lean/) | Formalizing math in Lean 4 |
| [Functional Programming in Lean](https://lean-lang.org/functional_programming_in_lean/) | Lean 4 as a programming language |
| [Learning Lean 4 (community)](https://leanprover-community.github.io/learn.html) | Curated learning resources |
| [Simons Institute Lean Tutorial (video)](https://www.classcentral.com/course/youtube-lean-tutorial-448087) | 58-min video introduction |

### Synchronous Dataflow & Reactive Verification

| Resource | What |
|----------|------|
| [Lustre Paper (PDF)](https://homepage.cs.uiowa.edu/~tinelli/classes/181/Spring08/Papers/Halb91.pdf) | Foundational: synchronous dataflow language |
| [Vélus: Verified Lustre Compiler](https://velus.inria.fr/) | Verified dataflow → assembly compiler |
| [Kind 2 Model Checker](https://kind2-mc.github.io/kind2/) | SMT model checker for Lustre programs |
| [BeepBeep Stream Verification](https://www.sciencedirect.com/science/article/pii/S0890540123000615) | Formal verification for event stream processing |
| [Verification of Reactive Systems (Schneider)](https://www.amazon.com/Verification-Reactive-Systems-Algorithms-Theoretical/dp/3642055559) | Comprehensive theory textbook |

### Verified Compilers

| Resource | What |
|----------|------|
| [CompCert Paper (PDF)](https://xavierleroy.org/publi/compcert-CACM.pdf) | Landmark: verified C compiler in Coq. 42K lines, 3 person-years, 76% is proof |
| [CompCert Homepage](https://compcert.org/) | Project overview and documentation |

### Practical Formal Methods (Practitioner Perspective)

| Resource | What |
|----------|------|
| [Hillel Wayne: Blog Posts on Formal Methods](https://www.hillelwayne.com/tags/formal-methods/) | Practical essays from a working consultant |
| [Hillel Wayne: Formal Methods in Practice (TLA+ at eSpark)](https://medium.com/espark-engineering-blog/formal-methods-in-practice-8f20d72bce4f) | Real-world TLA+ use case |
| [Learn TLA+ (free online)](https://learntla.com/) | Accessible TLA+ guide by Wayne |
| [Practical TLA+ (book)](https://www.amazon.com/Practical-TLA-Planning-Driven-Development/dp/1484238281) | Comprehensive TLA+ book |

### Lightweight Formal Methods (Alloy)

| Resource | What |
|----------|------|
| [Formal Software Design with Alloy 6 (free online)](https://haslab.github.io/formal-software-design/overview/index.html) | Modern, free Alloy 6 tutorial |
| [Software Abstractions — Daniel Jackson](https://www.amazon.com/Software-Abstractions-Logic-Language-Analysis/dp/0262017156) | Classic Alloy textbook |

### Property-Based Testing (Bridge to Formal Methods)

| Resource | What |
|----------|------|
| [Foundational Property-Based Testing (QuickChick/Coq)](https://lemonidas.github.io/pdf/Foundational.pdf) | Verified QuickCheck in Coq — PBT meets proofs |

---

## Summary Table: Critical Path Only

| Phase | # | Resource | Format | Time | Goal | Boon Design Impact |
|-------|---|----------|--------|------|------|--------------------|
| 0 | 0a | Kleppmann: AI + FV | Blog | 15 min | A | Frames the vision |
| 0 | 0b | ZipCPU Blog | Blog | 30 min | A | Motivation: FV finds real bugs |
| 1 | 1a | Natural Number Game | Browser | 2-3 hrs | A | First proof experience |
| 1 | 1b | SPARK Webinar Video | Video | 1 hr | A | Visual intro to verified language |
| 1 | 1c | **Lustre Paper** | Paper | 1 day | A | **Grounds everything in dataflow** |
| 2 | 2a | **Program Proofs (Dafny)** | Book | 3-4 wks | A | **Core: proof annotation syntax** |
| 2 | 2b | SPARK Book | Book | 1-2 wks | A | Graduated verification levels |
| 3 | 3a | AutoVerus Paper | Paper | 1 day | A | AI proof generation blueprint |
| 3 | 3b | DafnyBench Paper | Paper | 1 day | A | AI verification benchmarking |
| 4 | 4a | Liquid Haskell Tutorial | Book | 1 wk | A | Refinement types for streams |
| 4 | 4b | F* (key insight only) | Summary | 30 min | A | Effects + verification |
| 4 | 4c | Programming Z3 | Guide | 1-2 wks | A | The verification engine |
| 4 | 4d | Verus Tutorial | Tutorial | 1-2 days | A | Verified systems language |
| 5 | 5a | Vélus Compiler | Tool | 1 day | A+B | Verified dataflow compiler |
| 5 | 5b | Kind 2 | Tool | 1 day | A | Model checker for dataflow |
| 5 | 5c | BeepBeep Paper | Paper | 1 day | A | Stream pipeline verification |
| 6 | 6 | K Framework | Framework | 2-3 wks | B | Boon's formal semantics |
| 7 | 7a | Lean 4 Guide | Textbook | 3-4 wks | A+B | Ceiling: proofs as first-class |
| 7 | 7b | Idris Book | Book | 2-3 wks | A | Practical dependent types |
| 7 | 7c | Rocq/Software Foundations | Books | 3-4 wks | A+B | Verified algorithms & PL theory |
| 8 | 8a | Learn TLA+ | Free book | 2-3 wks | B | Engine concurrency specs |
| 8 | 8b | TLA+ (Lamport) | Book | 3-4 wks | B | Comprehensive specification |
| 8 | 8c | Alloy 6 | Free book | 1-2 wks | B | Lightweight design modeling |
| 9 | 9a | HW Verification (Springer) | Textbook | 3-4 wks | A | FPGA verification theory |
| 9 | 9b | Reactive Systems (Schneider) | Textbook | 3-4 wks | A+B | Reactive system theory |
| SupGen | — | SupGen gists + Interaction Calculus | Gists/Repo | ~4 hrs | A | Symbolic synthesis alternative to Z3 |
| SupGen | — | Kind proof language | Repo | 1 hr | A | CoC+Self type theory for Layer 3 |

---

## Key Design Questions for Boon

### How much proof burden falls on the user (or AI) vs. the compiler?

- **Dafny model:** User writes contracts → compiler proves them via SMT solver. AI-friendly because contracts are small annotations.
- **Lean model:** User writes proofs as code → compiler checks them. More powerful but harder for AI.
- **Refinement type model** (Liquid Haskell/F*): Types carry predicates → compiler infers most proofs. Least annotation needed, but limited in what you can express.
- **Lustre/Kind 2 model:** Programs and properties in the same language → model checker verifies automatically. No separate proof language needed.

- **SupGen/NeoGen model** (HVM4/Bend2): Programmer writes partial code with holes → symbolic search fills them with proven-correct code. No proofs needed at all — the synthesized code is correct by construction. See the "Symbolic Program Synthesis" section for details.

For AI generation, the sweet spot is probably **Dafny-style contracts + automatic discharge** (like SPARK's Bronze/Silver levels). The AI generates `ENSURES` and `REQUIRES` annotations, and Boon's compiler uses Z3 to verify them automatically. When automatic proving fails, the AI can fall back to writing explicit proof hints (like Dafny's `calc` blocks or Lean's tactics).

### What kind of properties matter most?

- **Value properties** (Hoare logic): "This stream only produces positive values." Best served by contracts and refinement types.
- **Temporal properties** (LTL/CTL): "If button pressed, UI eventually updates." "Counter is always >= 0." Best served by model checking (Kind 2 approach).
- **Dataflow properties** (information flow): "User passwords never reach the logging module." Best served by SPARK-style flow analysis.

Boon will likely need all three, but **temporal properties are the most natural fit** for a reactive language. The Lustre/Kind 2 model — same-language properties + model checking — should be the primary verification approach, with Hoare-logic contracts as a supplement for function-level specifications.

### When does verification happen?

The four-layer model provides a smooth adoption path:
- **Layer 0 (runtime monitoring):** Ship first. Immediate value. No compiler changes needed beyond adding assertion checks to streams.
- **Layer 1 (structural inference):** Ship with the type system. WHEN guards → refinement types, HOLD → state invariants. No user annotations needed.
- **Layer 2 (static assertions):** Ship when Z3 integration is ready. `ASSERT` becomes a compile-time check.
- **Layer 3 (explicit proofs):** Ship last. Only needed for critical properties where automatic proving fails.

Boon's dataflow nature means many properties (like "this stream only produces positive values") could be checked by **refinement types on streams** — requiring zero explicit annotations. That's the dream: most code is verified automatically, contracts handle the rest, and explicit proofs are rare.

The Lustre/Kind 2 insight adds another dimension: since Boon programs are dataflow compositions (like Lustre), Boon's own constructs may be expressive enough to state verification properties — no separate annotation language needed. A `WHEN` guard is already a precondition. A `HOLD` body already describes a state transition. The verification could emerge from the language itself. This idea is explored in depth below.

---

## Boon Constructs as Specifications: The Intrinsic Verification Thesis

This section describes Boon's most distinctive verification idea. In the programming languages community, this approach is known as **"correct by construction"** or **"making illegal states unrepresentable"** — a principle championed by languages like Elm, Rust (ownership/borrowing prevents data races by construction), and typed functional languages (algebraic data types prevent impossible states). Boon extends this principle to reactive dataflow: the language constructs themselves encode verification properties, so many correctness guarantees emerge from the program structure without any annotations.

Most verified languages have a **two-language problem**: you write your program in language A, then write specifications about it in language B (annotations, contracts, proof scripts). The specification language is always an afterthought — bolted on. But Boon's constructs already encode the *structure* of specifications: guards, state transitions, data flow constraints. The question is whether the compiler can *extract* verification properties from what's already written, rather than requiring the programmer to restate them as annotations.

### The Two-Language Problem Illustrated

In Dafny, you write a program and then annotate it:

```
method Increment(count: int) returns (r: int)
  requires count >= 0        // ← separate specification
  ensures r >= 0             // ← separate specification
  ensures r == count + 1     // ← separate specification
{
  r := count + 1;            // ← the program
}
```

There are **four lines of spec** for **one line of program**. The spec and program are separate things that must be kept in sync.

Now look at the equivalent Boon:

```boon
counter: 0 |> HOLD counter {
    increment_button.event.press |> THEN { counter + 1 }
}
```

This tiny program already contains, *structurally*, everything the Dafny annotations say:

| Dafny annotation | Boon structural equivalent |
|---|---|
| `requires count >= 0` | The initial value `0` and the transition `counter + 1` (which only increases) |
| `ensures r >= 0` | Inferable: starts at 0, only adds 1 → always >= 0 |
| `ensures r == count + 1` | The body `counter + 1` IS the transition function |

The program **is** the specification. There's nothing to add.

### Example 1: WHEN Guards Are Preconditions

Consider input validation in a traditional verified language vs. Boon:

**Dafny approach** — specification is separate from logic:
```
method ProcessTemperature(raw: real)
  requires raw > -273.15      // absolute zero
  requires raw < 1000.0       // sensor range
  ensures result > -273.15
{
  // ... process ...
}
```

**Boon approach** — the WHEN guard IS the precondition:
```boon
valid_temperatures: raw_sensor |> WHEN {
    t WHERE t > -273.15 AND t < 1000 => t
    __ => SKIP
}

alarm: valid_temperatures |> WHEN {
    t WHERE t > 100 => Alert(TEXT { High: {t} })
}
```

What the compiler can **infer without any annotations**:

1. `valid_temperatures` is a stream of `{ t | -273.15 < t < 1000 }` — the WHEN guard defines this refinement type automatically
2. `alarm` receives values where `t > 100` — but also, since it consumes `valid_temperatures`, the compiler knows `t > -273.15 AND t < 1000` still holds
3. Anything downstream of `valid_temperatures` inherits the guarantee — no need to restate it

The WHEN pattern `t WHERE t > -273.15 AND t < 1000` is simultaneously:
- **The program** (filter invalid readings)
- **A precondition** (downstream consumers only see valid values)
- **A refinement type** (the stream's type is narrowed)

### Example 2: HOLD Initial Value + Body Define a State Machine Invariant

```boon
counter: 0 |> HOLD counter {
    LATEST {
        increment_button.event.press |> THEN {
            counter |> WHEN { c WHERE c < 100 => c + 1, __ => SKIP }
        }
        decrement_button.event.press |> THEN {
            counter |> WHEN { c WHERE c > 0 => c - 1, __ => SKIP }
        }
    }
}
```

What the compiler can infer:

1. **Initial state**: `counter = 0`, so `0 <= counter <= 100` holds initially (trivially: `counter == 0`)
2. **Transitions**: increment only fires when `c < 100` (WHEN guard), decrement only when `c > 0` (WHEN guard)
3. **Invariant**: `0 <= counter <= 100` — provable by induction:
   - Base case: initial value is 0, which satisfies `0 <= 0 <= 100`
   - Inductive step: if `0 <= counter <= 100` holds, then:
     - Increment: fires only when `counter < 100`, produces `counter + 1 <= 100`
     - Decrement: fires only when `counter > 0`, produces `counter - 1 >= 0`

**No annotations needed.** The HOLD initial value is the base case. The WHEN guards in the body are the transition guards. The invariant emerges from the structure.

In Dafny you'd have to write `invariant 0 <= counter <= 100` explicitly. In Boon, the compiler can derive it.

### Example 3: WHILE Patterns Are Loop Invariants

```boon
connection_status |> WHILE {
    Connected(socket) => socket |> read_data() |> process()
    Disconnected => reconnect_ui()
}
```

During the `Connected` arm's body, the compiler **knows** `socket` is valid — the WHILE pattern `Connected(socket)` destructures the value and proves the socket exists. This is equivalent to a loop invariant saying "while in this branch, the socket is connected."

If `read_data()` had a precondition "socket must be connected," it's satisfied by construction — the WHILE pattern guarantees it.

### Example 4: Pipe Chains Are Proof Chains

```boon
title_to_add:
    elements.new_todo_title_text_input.event.key_down.key
    |> WHEN { Enter => BLOCK {
        trimmed: elements.new_todo_title_text_input.text |> Text/trim()
        trimmed |> Text/is_not_empty() |> WHEN { True => trimmed, False => SKIP }
    }}

todos: LIST {}
    |> List/append(item: title_to_add |> new_todo())
```

Follow the pipe chain and track what the compiler knows at each stage:

| Stage | What passes through | What the compiler knows |
|---|---|---|
| `event.key_down.key` | Any key | It's a key event |
| `\|> WHEN { Enter => ... }` | Only Enter keypresses | Key == Enter |
| `Text/trim()` | Trimmed text | Leading/trailing whitespace removed |
| `Text/is_not_empty() \|> WHEN { True => ... }` | Non-empty trimmed text | `text.length > 0` |
| `\|> new_todo()` | A new todo | Has non-empty title guaranteed |

By the time `title_to_add` reaches `List/append`, the compiler knows it's a non-empty, trimmed string from an Enter keypress. Each pipe stage **narrows the type**. No annotations required — each `WHEN` filter and each function in the chain adds a guarantee.

### Example 5: LATEST Expresses Relationships

```boon
store: [
    counter: 0 |> HOLD counter {
        LATEST {
            increment_button.event.press |> THEN { counter + 1 }
            decrement_button.event.press |> THEN { counter - 1 }
        }
    }
    doubled: counter * 2
]
```

`doubled` is defined as `counter * 2`. This isn't just a computation — it's a **relational invariant**: `doubled == counter * 2` holds at all times. The compiler knows this by construction. If anything downstream depends on the relationship between `counter` and `doubled`, it's provable without annotations.

### Four Layers of Verification

These examples reveal four natural verification layers, from easiest to implement to most powerful:

**Layer 0: Runtime monitoring (stream assertions at runtime)**

Before any static analysis exists, Boon can check assertions on stream values as they flow at runtime:
```boon
valid_temperatures: raw_sensor
    |> ASSERT { t => t > -273.15 AND t < 1000 }  -- checked at runtime
    |> WHEN { t WHERE t > 100 => Alert(TEXT { High: {t} }) }
```
This is trivial to implement (add assertion checks to stream processing), immediately useful (catches bugs during development), and provides a smooth upgrade path: the same assertions later become static proof obligations when the static verifier is built. Lustre's [Lesar](https://www.di.ens.fr/~pouzet/cours/mpri/bib/lesar-rapport.pdf) (a BDD-based model checker for Lustre safety properties) and BeepBeep both support this pattern. **This could be Boon's first verification feature — shipped before any static analysis.**

**Layer 1: Free (structural inference, no annotations)**

The compiler infers properties from the program structure:
- WHEN/WHILE guards → refinement types on streams (preconditions)
- HOLD initial value + guarded transitions → state invariants
- Pipe chains → cumulative type narrowing
- Variable definitions → relational invariants

**Layer 2: Lightweight (optional assertions for non-obvious properties)**

For properties that can't be inferred from structure alone:
```boon
counter: 0 |> HOLD counter {
    complex_logic |> THEN { new_value }
}
ASSERT counter >= 0  -- only needed when inference can't prove it
```

**Layer 3: Full proofs (for critical correctness guarantees)**

For properties that need explicit reasoning:
```boon
-- Hypothetical syntax for explicit proof hints
counter: 0 |> HOLD counter {
    ...
}
PROVE counter >= 0 BY INDUCTION {
    BASE: counter == 0, so counter >= 0
    STEP: if counter >= 0, then counter + 1 >= 0
}
```

### Why This Matters

**Layer 1 is already huge.** In most languages, you get zero verification for free — everything must be annotated. But Boon's reactive constructs are so structured that the compiler can infer refinement types, state invariants, and data flow guarantees from the program itself. This is exactly what makes Boon ideal for AI generation: the AI writes natural Boon code, and most correctness properties are verified automatically. Layer 2 (static assertions) handles the rest. Layer 3 (full proofs) is rarely needed. And the smooth progression — Layer 0 → 1 → 2 → 3 — means each step delivers value independently.

This approach is similar to how Lustre works — programs and properties in the same language — but Boon's richer construct set (HOLD, WHEN, WHILE, LATEST, LINK, pipes) gives the compiler even more structure to reason about. And it's similar to refinement type inference in Liquid Haskell, but arising from the language constructs rather than type annotations.

---

## What to Build First

After reading through the resources, here's a suggested implementation order for bringing verification to Boon:

**Step 1: Layer 0 — Runtime stream assertions** (implementable today)

Add an `ASSERT` construct that monitors stream values at runtime:
```boon
counter: 0 |> HOLD counter {
    increment_button.event.press |> THEN { counter + 1 }
}
ASSERT counter >= 0  -- fails loudly at runtime if violated
```
This requires no static analysis — just insert checks into the stream processing pipeline. Immediate value for debugging and for AI-assisted development (the AI adds assertions, you run the program, violations give feedback).

**Step 2: Layer 1 proof of concept — WHEN guard refinement inference**

Implement flow-sensitive type narrowing for WHEN guards in the compiler. After:
```boon
valid: input |> WHEN { x WHERE x > 0 => x }
```
The compiler tracks that `valid` has type `{ x | x > 0 }`. Downstream consumers can rely on this. This is the core of the intrinsic verification thesis and doesn't require SMT — just dataflow analysis.

**Step 3: Semantic foundation — Define core constructs in K Framework**

Formally define HOLD, WHEN, WHILE, LATEST, THEN, LINK in K. This gives a reference semantics and a deductive verifier. Use it to verify that the Actors, DD, and WASM engines all produce the same results for the same programs (Goal B).

**Step 4: Layer 1 full — HOLD invariant inference**

Extend the compiler to infer state invariants for HOLD constructs using the initial value + guarded transitions pattern. Back this with Z3 for automatic proof discharge.

**Step 5: Layer 2 — Static ASSERT with Z3**

Turn runtime `ASSERT` into static proof obligations. The compiler generates verification conditions from assertions and sends them to Z3. Assertions that Z3 can discharge are verified at compile time; others remain as runtime checks (graceful degradation).

**Step 6: Temporal properties — Kind 2-style model checking**

Add temporal property checking for Boon programs, following Kind 2's approach: BMC + k-induction + IC3 over the dataflow graph. This enables safety and liveness properties ("the counter is always >= 0," "if button pressed, UI eventually updates").

**Step 7: Symbolic synthesis — SupGen/NeoGen hole-filling** (optional, experimental)

Integrate HVM4's superposition-based program synthesis as an alternative proof/code search engine. Where Z3 tries to *decide* whether a property holds, SupGen *searches* for a program (or proof term) that makes it hold. See the "Symbolic Program Synthesis" section below for the full picture.

---

## Symbolic Program Synthesis: HVM4, Bend2, SupGen, NeoGen

This section describes a fundamentally different approach to verification and code generation — one based on **interaction nets and superpositions** rather than SMT solvers and type inference. It comes from [Victor Taelin](https://github.com/VictorTaelin) and [Higher Order Company](https://higherorderco.com/), creators of HVM, Bend, and Kind. Boon already has research on HVM as a compilation target (`docs/language/gpu/HVM_BEND_ANALYSIS.md`); this section adds the **verification and program synthesis** angle.

### The Core Idea: Superposition-Based Search

Traditional program synthesis tries candidates one-by-one. SupGen does something different: it merges ALL candidates into a single **superposed** term and evaluates them simultaneously, sharing computation across overlapping sub-expressions.

The mechanism comes from the [Interaction Calculus](https://github.com/VictorTaelin/Interaction-Calculus), which extends lambda calculus with two primitives:

- **SUP (superposition):** Combines multiple values into one location. `{10, 20}` is a superposition of 10 and 20.
- **DUP (duplication):** Splits a superposition back into its components.

When you apply a superposed function to an argument, HVM evaluates ALL branches simultaneously, sharing any identical sub-computations:

```
(+ {10, 20} 1) → {11, 21}     -- both additions share the same "add 1" step
```

This turns brute-force enumeration into shared evaluation. [In the ADD-CARRY benchmark](https://gist.github.com/VictorTaelin/d5c318348aaee7033eb3d18b0b0ace34), finding a 16-bit binary addition algorithm took ~262M interactions brute-force but only ~36K with superpositions — a **7,277× speedup**, with less than 1 interaction per candidate.

### The Tools

**[HVM4](https://x.com/VictorTaelin/status/1985320306001477783)** — The runtime. Now 100% C, compiles Interaction Calculus functions (including superpositions) to zero-overhead machine code. Has a [native type system running directly on interaction nets](https://x.com/VictorTaelin/status/1971591584916393984), making it a fast, parallel proof verifier. 130-160M interactions/sec on a single core. Taelin claims it could be "OOMs faster than Lean" for proof checking.

**SupGen** — The synthesis engine, [now built into HVM4](https://x.com/VictorTaelin/status/1971591584916393984). A model-free (no neural network) program synthesizer. Given a type signature or input/output examples, it creates a superposition of all candidate programs, evaluates them against constraints, and collapses to the solutions. [Performance](https://news.ycombinator.com/item?id=42771885): up to 1000× faster than existing synthesizers for certain benchmarks. Can now [enumerate types as well as terms](https://x.com/VictorTaelin/status/1978536555460386879).

**[NeoGen](https://x.com/VictorTaelin/status/1957775213053022614)** — The user-facing layer in [Bend2](https://x.com/VictorTaelin/status/1933621356408500461). A "program/proof miner" that lets you put a **hole** anywhere in your code, and the compiler fills it so all tests and proofs pass. Can be seen as ["denoising proofs with holes"](https://x.com/VictorTaelin/status/1946965897513439654): take a complete proof, remove sub-expressions, NeoGen recovers the lost bits. [Found every primitive recursive function tested instantly](https://x.com/VictorTaelin/status/1904727018899439853): equality in 0.0008s, DrawLine 0.001s, Insert 0.006s.

**[Kind](https://github.com/HigherOrderCO/Kind)** — Taelin's proof language. A minimal proof checker based on CoC+Self (Calculus of Constructions + Self types). Dependent types and theorem proving, targeting HVM for execution. The type checker runs on interaction nets — parallel proof checking.

**[Bend2](https://x.com/VictorTaelin/status/1933621356408500461)** — The next version of Bend. Not yet released, but planned: GPU compilation, lazy HVM compilation, JS/Python compilation, complete proof system (like Lean/Kind), NeoGen built-in, dependent types. [Raising $4M to complete it](https://wefunder.com/higher.order.co).

### Three Connections to Boon

#### 1. Execution Target (extends existing HVM research)

Boon already explores HVM as a GPU compilation target (`docs/language/gpu/HVM_BEND_ANALYSIS.md`). HVM4 strengthens this: compiled mode with zero overhead, 100% C codebase, superposition support. The Actor Model → Interaction Net mapping is even more viable now.

#### 2. Proof Verification Backend

HVM4's native type system on interaction nets could serve as Boon's proof checker — an alternative (or complement) to Z3. Where Z3 works in the theory of first-order logic + arithmetic, HVM4 works in the theory of interaction combinators. Different strengths:

| | Z3 (SMT) | HVM4 (Interaction Nets) |
|---|---|---|
| **Strengths** | Decidable theories (linear arithmetic, arrays, bitvectors), industrial maturity | Massive parallelism, optimal sharing, exponential speedups for structural proofs |
| **Weaknesses** | Undecidable for general programs, sequential | Exponential for non-structural problems, immature |
| **Best for** | Arithmetic invariants, refinement type checking | Inductive proofs, type-level reasoning, program synthesis |
| **Maturity** | 15+ years, powers Dafny/SPARK/Verus | Experimental, HVM4 just released |

For Boon, the practical path: **start with Z3** (proven, well-understood from Phases 2-4 of this guide), **explore HVM4 later** once its type system matures. But Kind's CoC+Self type theory is worth studying now as a model for Boon's Layer 3 dependent types.

#### 3. Symbolic Program Synthesis (the new angle)

This is where SupGen/NeoGen diverge from everything else in this guide. The standard approach from Phases 0-5 is:

> Programmer (or AI) writes code + annotations → Compiler checks them via SMT → Feedback loop

SupGen/NeoGen add a third strategy:

> Programmer writes **partial code with holes** → SupGen searches for completions that satisfy constraints → Proven correct by construction

This is orthogonal to both LLMs and SMT solvers:

| | LLMs | SMT (Z3) | SupGen |
|---|---|---|---|
| **What it does** | Generates code from natural language | Checks whether a property holds | Searches for code/proofs that satisfy constraints |
| **Guarantee** | Probabilistic (may be wrong) | Sound (if it says yes, it's true) | Sound (found programs satisfy all tests/types) |
| **Scalability** | Works on large programs | Depends on theory decidability | Exponential, but huge constant-factor speedup |
| **Best for** | Whole-program generation, boilerplate | Checking arithmetic invariants | Filling small holes, finding proofs, synthesizing predicates |

### How This Could Look in Boon

**Hole-filling for WHEN predicates:**

```boon
-- Developer writes the structure, leaves the predicate as a hole:
valid_input: raw |> WHEN {
    x WHERE ??? => x        -- ← hole: what predicate?
    __ => SKIP
}

-- Given examples: raw=5 → valid_input=5, raw=-1 → valid_input=SKIP, raw=0 → valid_input=SKIP
-- SupGen discovers: ??? = x > 0
```

**Hole-filling for HOLD transitions:**

```boon
counter: 0 |> HOLD counter {
    button.event.press |> THEN { ??? }  -- ← hole: what transition?
}

-- Given: press,press,press → counter outputs 0,1,2,3
-- SupGen discovers: ??? = counter + 1
```

**Proof search for invariants:**

```boon
counter: 0 |> HOLD counter {
    LATEST {
        inc.event.press |> THEN { counter + 1 }
        dec.event.press |> THEN { counter - 1 }
    }
}
ASSERT counter >= 0  -- Can SupGen find why this might fail?
```

Here SupGen could search for a **counterexample** (a sequence of events that violates the assertion) or a **proof** (an inductive argument that it holds). In this case, it would find the counterexample: press dec first → counter becomes -1. This is complementary to Z3, which could verify the same property but through a different mechanism.

**Combined workflow: LLM + SupGen + Z3:**

1. **LLM** generates the overall program structure (layout, data flow, HOLD/WHEN/WHILE skeleton)
2. **SupGen** fills small holes (predicates, transition functions, helper logic) with guaranteed-correct code
3. **Z3** verifies global properties (state invariants, temporal assertions) that span multiple constructs
4. If Z3 fails, **SupGen** searches for proof terms; if SupGen times out, **LLM** generates proof hints

This three-engine approach plays to each tool's strengths: LLMs for large-scale structure, SupGen for small-scale synthesis, Z3 for verification.

### Honest Limitations

1. **Boon is reactive/stateful, SupGen needs pure functions.** Superpositions require deterministic evaluation — same input, same output. Boon's streams, DOM events, and actor state can't be directly superposed. The practical approach: decompose Boon programs into pure functional cores (WHEN predicates, HOLD transitions, pipe transformations) + reactive wiring, and use SupGen only on the pure parts.

2. **Exponential is still exponential.** Despite 7000× constant-factor speedups, the search space grows exponentially with program size. SupGen works brilliantly for small programs (primitive recursive functions in milliseconds) but won't synthesize an entire todo app. Sweet spot: individual WHEN predicates, HOLD transitions, and pipe transformations — exactly the "holes" in the examples above.

3. **Bend2 and HVM4 are not yet production-ready.** Bend2 hasn't shipped. HVM4's type system is new. Building Boon's core verification on unreleased technology is risky. But the *ideas* (superposition search, hole-filling UX, proofs on interaction nets) can inform Boon's design now, with implementation following when the tools mature.

4. **Different "Kind 2" projects — don't confuse them.** Phase 5b of this guide covers [Kind 2 the model checker](https://kind2-mc.github.io/kind2/) (University of Iowa, for Lustre programs). Taelin's [Kind](https://github.com/HigherOrderCO/Kind) is a completely separate project — a proof language. Both are relevant to Boon but for different reasons.

### Reading Resources

| Resource | Type | Time | Why Read It |
|---|---|---|---|
| [Fast Discrete Program Search with HVM Superpositions](https://gist.github.com/VictorTaelin/d5c318348aaee7033eb3d18b0b0ace34) | Technical gist | 30 min | Core mechanism: how SUP nodes give 7000× speedup. The ADD-CARRY example makes the idea concrete. |
| [Accelerating DPS with SUP Nodes](https://gist.github.com/VictorTaelin/7fe49a99ebca42e5721aa1a3bb32e278) | Technical gist | 30 min | Deeper explanation with Haskell comparison and theoretical analysis. |
| [Interaction Calculus (GitHub)](https://github.com/VictorTaelin/Interaction-Calculus) | Repo + README | 1 hour | Foundation: superpositions, duplications, optimal evaluation. Lambda Calculus + Interaction Combinators. |
| [Kind proof language (GitHub)](https://github.com/HigherOrderCO/Kind) | Repo | 1 hour | Minimal proof checker on HVM (CoC+Self). Model for Boon's Layer 3 dependent types. |
| [SupGen HN discussion](https://news.ycombinator.com/item?id=42771885) | Discussion | 20 min | Community analysis, limitations, comparisons to classical theorem provers. |
| [HVM4 + SupGen + native types (tweet)](https://x.com/VictorTaelin/status/1971591584916393984) | Announcement | 5 min | Native type system on interaction nets, proof verification claims. |
| [Bend2 recap: NeoGen + proof mining (tweet)](https://x.com/VictorTaelin/status/1957775213053022614) | Overview | 10 min | Hole-filling, dependent types, program/proof mining integrated. |
| [NeoGen as denoising proofs (tweet)](https://x.com/VictorTaelin/status/1946965897513439654) | Insight | 5 min | The "denoising proofs with holes" mental model. |
| [Boon's existing HVM research](docs/language/gpu/HVM_BEND_ANALYSIS.md) | Internal doc | 30 min | Review Boon's HVM compilation target analysis before reading HVM4 material. |

**Suggested reading order:** Start with the Boon HVM research doc (you wrote it), then the two SupGen gists (they're the technical core), then the Interaction Calculus README (theory), then Kind (proof language), then the tweets and HN discussion (context). Total: ~4 hours.

### Design Takeaway

SupGen/NeoGen represent a **fifth verification strategy** orthogonal to the four layers described earlier. Where Layer 1 *infers* properties from structure and Layer 2 *checks* assertions via Z3, SupGen *searches* for code and proofs that satisfy constraints. This is particularly powerful for:

- **Layer 1 acceleration:** Where the compiler needs to infer invariants from HOLD/WHEN/WHILE, SupGen could discover them by superposing all possible invariants and collapsing to the ones that hold.
- **Layer 3 proof search:** Where explicit proofs are needed, SupGen could find proof terms automatically, avoiding the proof burden problem entirely.
- **AI-assisted development:** Instead of LLMs generating whole programs (which need verification), SupGen fills verified holes in developer-written skeletons (which are correct by construction).

Taelin's observation that ["LLMs are BAD at proving theorems"](https://x.com/VictorTaelin/status/1945497309573251320) aligns with this guide's emphasis on automated verification over manual proof. SupGen offers a third path: neither human-written proofs (expensive) nor LLM-generated proofs (unreliable), but **symbolically searched proofs** (sound, automated, but limited in scale).

---

## HVM/Bend as a Boon Engine

Beyond verification and program synthesis, there's a deeper question: could HVM/Bend become a **full execution engine** for Boon — alongside the existing Actors, DD, and planned WASM engines?

### Global Lambdas → Actors → An Erlang on Interaction Nets

The Interaction Calculus has a unique feature: **global lambdas** (also called "unscopped lambdas" or "scopeless lambdas"). Unlike normal lambda calculus where variables are scoped to their lambda body, IC variables can appear anywhere in the program. In a [key tweet](https://x.com/VictorTaelin/status/1784215941045191065), Taelin explained how to **emulate Erlang's entire Actor Model on HVM-CUDA** with pure expressions:

> "HVM has a feature that goes beyond the functional paradigm: unscopped lambdas. Or, more like: lambdas whose variables can occur outside their bodies. With these, we can essentially emulate channels and, thus, implement any concurrent algorithm. [...] By just implementing that as pure expressions, we'd be able to implement a full Erlang-like environment on GPUs, without ever touching a single lock or mutex."

He demonstrated a ping-pong between two "threads" where each thread is a recursive function that handles a message and passes a channel (via unscopped lambda) to receive the response. The [full example code is on GitHub](https://gist.github.com/VictorTaelin/3124a19d340dad2f97f44353f74c6e41).

Global lambdas unlock three capabilities that together form an actor model:

**1. Pure mutable references.** A scopeless lambda `λ$count 0` creates a global "slot" `$count`. Applying `(λ$count (old_count + 1))(old_count)` atomically reads the old value and writes a new one — like `std::mem::replace` in Rust, but as an interaction net rewrite. See Taelin's gists on [pure mutable references](https://gist.github.com/VictorTaelin/fb798a5bd182f8c57dd302380f69777a) for the full mechanism.

**2. Continuations.** Scopeless lambdas can capture execution contexts, giving you `call/cc` (call-with-current-continuation). This enables yield/resume, cooperative multitasking, and green threads — all within a pure functional runtime.

**3. Actor-like processes.** Combine mutable references (for channels) with continuations (for message passing) and you get Erlang-style actors. Here's the actual HVM1 code from the tweet — a ping-pong between two "threads":

```hvm
(If 0 t f) = f
(If x t f) = t

// Actor handler for "PING" messages
(Handle self (PING tick answer)) =

  // Answers with another "PING"
  // - sends a "channel" to get next response: λ$res
  // - gets the other thread's name
  let other = (answer (PING (- tick 1) λ$res(self)))

  // Logs a message to console
  (Log self ["got ping from", other, tick]

  // If tick > 1...
  (If (> tick 1)
      // Handles the response
      (Handle self $res)
      // Otherwise, just quit
      Done))

(Main) =
  // Initial message: B sends PING to A
  let message  = (PING 4 λ$res("B"))

  // Thread A handles the initial message
  let thread_A = (Handle "A" message)

  // Thread B handles the initial response
  let thread_B = (Handle "B" $res)

  // Create both threads in parallel
  (Pair
    (Thread "A" thread_A)
    (Thread "B" thread_B))
```

The key mechanism: `λ$res` is an unscopped lambda — its variable `$res` appears *outside* its body (in `thread_B`). This creates a **channel**: when thread A calls `answer` with a new PING, the response flows into `$res`, which thread B is waiting on. No locks, no mutexes, no channels library — just interaction net rewiring. And it runs on GPUs via CUDA.

The output shows interleaved messages between threads A and B, exactly like Erlang process communication:
```
Thread "A": got ping from "B", 4 → got ping from "B", 2 → got ping from "B", 0 → Done
Thread "B": got ping from "A", 3 → got ping from "A", 1 → Done
```

**Why this works:** The `Main` function creates `(PING 4 λ$res("B"))` — where `λ$res` is an unscopped lambda. Then `thread_B = (Handle "B" $res)` uses `$res` *outside* that lambda's body. In normal lambda calculus this is illegal. In the Interaction Calculus, it creates a **communication channel**: `$res` becomes a "wire" in the interaction net that connects thread A's response to thread B's input. When HVM reduces the net, the `answer` call in `Handle` writes a value into `$res`, and `thread_B` reads it out — all through pure graph rewriting. This is why Taelin can claim "Erlang on GPUs without locks" — the interaction net's local rewrite rules guarantee that the channel connects exactly one writer to one reader, with ordering preserved by the recursion structure.

**Mapping to Boon:** `HOLD` = actor state, `THEN`/`WHEN` = message handler, unscopped lambda = channel between actors. The compilation path would be Boon constructs → HVM interaction net nodes.

### What This Means for Boon

Boon already has extensive research on this topic: `docs/language/gpu/HVM_ACTORS_RESEARCH.md` (a 10-experiment research plan) and `docs/language/gpu/HVM_BEND_ANALYSIS.md` (compilation target analysis). The key question is: **when and how does this become a real engine?**

**Current Boon engines and where HVM fits:**

| Engine | Target | Strengths | Status |
|---|---|---|---|
| **Actors** | Browser/WASM | Reactive streams, DOM integration | Production |
| **DD** | Browser/WASM | Incremental computation | Merged |
| **WASM** | Browser/WASM | Direct compilation, no runtime overhead | Planned |
| **HVM/Bend** | CPU/GPU (+ WASM via C→WASM?) | Massive parallelism, optimal evaluation, native proof verification, SupGen | Future research |

**Three realistic paths for an HVM engine:**

**Path A: HVM as compute backend (near-term).** Keep Actors/DD/WASM for browser UI. Use HVM for heavy computation: large list processing, data transformations, proof verification. Boon programs have two zones — reactive UI (browser engine) and pure compute (HVM engine). Communication via message passing across the boundary.

**Path B: HVM as full non-browser engine (medium-term).** Boon AST → Interaction Calculus compilation. Scopeless lambdas for actors, continuation-based scheduler for reactivity. Targets CPU/GPU natively. This would be the "server-side Boon" or "GPU Boon" engine — not for browsers initially, but for backend services, data processing, and FPGA/RISC-V targets.

**Path C: HVM-in-WASM (long-term, speculative).** HVM4 is 100% C, and C compiles to WASM. A Boon → IC → HVM4 → WASM pipeline could unify all targets under one engine. But this is the most complex path and depends on HVM4's WASM performance being competitive.

**The verification connection makes this more compelling than "just another engine."** Without verification, an HVM engine is interesting but optional. With verification, the picture changes: HVM4's native type system + SupGen could make Boon the first language where **the same runtime that executes your program also verifies it** — not "compile, then verify, then run" but a single interaction net that evaluates code, checks types, and searches for proofs simultaneously.

### What to Do Now

**Revisit this after Bend2 is officially released.** Bend2 is still in development ([raising $4M to complete it](https://wefunder.com/higher.order.co)). The ideas are sound, but building a Boon engine on unreleased infrastructure would be premature. Once Bend2 ships with its proof system, NeoGen, and dependent types, we can:

1. **Re-evaluate the compilation path.** Does Boon → Bend2 make sense, or should Boon target HVM4's IC directly?
2. **Test scopeless lambda actors.** Run the experiments from `docs/language/gpu/HVM_ACTORS_RESEARCH.md` on HVM4 (not HVM2/3 which the doc targets). HVM4's compiled mode changes the performance picture.
3. **Prototype the verification integration.** Try expressing Boon's HOLD/WHEN/WHILE invariants as Kind type-level proofs on HVM4.
4. **Benchmark HVM4-WASM.** Compile HVM4's C to WASM and measure overhead vs Boon's native WASM engine.

Until then, the existing Actors/DD/WASM engines remain the priority, and the HVM research docs capture the design space for future exploration.

### Additional Reading for This Section

| Resource | Type | Why Read It |
|---|---|---|
| [Taelin: How to Emulate Erlang on HVM-CUDA (tweet)](https://x.com/VictorTaelin/status/1784215941045191065) | Tweet | The original insight: unscopped lambdas → channels → full Erlang Actor Model on GPUs |
| [Ping-pong actor example (gist)](https://gist.github.com/VictorTaelin/3124a19d340dad2f97f44353f74c6e41) | Code example | Working HVM1 code showing two "threads" communicating via unscopped lambda channels |
| [Optimal Linear Context Passing (gist)](https://gist.github.com/VictorTaelin/fb798a5bd182f8c57dd302380f69777a) | Technical gist | How scopeless lambdas implement pure mutable references |
| [Boon's HVM Actors Research](docs/language/gpu/HVM_ACTORS_RESEARCH.md) | Internal doc | 10-experiment plan for building actors on HVM |
| [Boon's HVM/Bend Analysis](docs/language/gpu/HVM_BEND_ANALYSIS.md) | Internal doc | Compilation target analysis for Boon → HVM |
| [Interaction Calculus (GitHub)](https://github.com/VictorTaelin/Interaction-Calculus) | Repo + README | Foundation: global lambdas, superpositions, affine variables |
