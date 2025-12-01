# HVM Actors Research Project

**Type:** Research & Exploration
**Goal:** Understand what's possible with HVM/Bend actor-like patterns and integration with Boon
**Status:** Planning Phase
**Date:** 2025-11-20

---

## Research Motivation

**Why explore HVM actors even if Rust actors are "better"?**

1. **Understand HVM's capabilities and limits**
   - What can scopeless lambdas actually do?
   - How far can we push continuations?
   - What's the performance envelope?

2. **Explore unique HVM features**
   - Scopeless lambdas don't exist in other languages
   - Interaction nets enable unique patterns
   - May discover novel concurrency approaches

3. **Inform Boon/HVM integration**
   - How should Boon code compile to HVM?
   - What patterns work well?
   - What should stay in Rust runtime vs HVM?

4. **Academic/scientific value**
   - Novel approach to concurrency
   - Could lead to papers/discoveries
   - Expand knowledge of interaction nets

5. **Future-proofing**
   - HVM3 may add features
   - Understanding foundation prepares for evolution
   - May influence HVM development

**Not about replacing Rust actors** - about understanding the design space!

---

## Research Questions

### Phase 1: Foundational Understanding

**Q1: What can scopeless lambdas actually express?**
- How do they work internally?
- What's the interaction net representation?
- What are the performance characteristics?

**Q2: How powerful are continuations in HVM?**
- Can we implement call/cc fully?
- What control flow patterns are possible?
- What are the limitations vs Scheme/Racket call/cc?

**Q3: Can we build cooperative multitasking?**
- Green threads via continuations?
- Yield/resume mechanism?
- How would scheduling work?

### Phase 2: Actor-Like Patterns

**Q4: Can we implement message passing?**
- Queue via mutable reference (scopeless lambda)?
- Send/receive primitives?
- What's the overhead?

**Q5: Can we implement actor state?**
- Isolated mutable state per "actor"?
- State updates via messages?
- Garbage collection behavior?

**Q6: Can we implement actor spawning?**
- Create new "actors" dynamically?
- Independent execution?
- How to coordinate?

### Phase 3: Performance & Practicality

**Q7: What's the performance profile?**
- Overhead vs Rust actors?
- Scalability (1, 10, 100, 1000 actors)?
- Memory usage?

**Q8: What breaks at scale?**
- Message queue length limits?
- Continuation stack depth?
- Garbage collection pressure?

**Q9: Can it leverage HVM's parallelism?**
- Do actor computations parallelize?
- Or does mutability kill parallelism?
- Hybrid approach possible?

### Phase 4: Integration with Boon

**Q10: How should Boon compile to HVM actors?**
- Syntax for actors in Boon?
- Compilation strategy?
- Type system implications?

**Q11: Rust actors vs HVM actors - when to use which?**
- Clear decision criteria?
- Hybrid patterns?
- Performance crossover points?

**Q12: Can we bridge Rust ↔ HVM actors?**
- Message passing between runtimes?
- Shared state?
- Supervision across boundary?

---

## Research Experiments

### Experiment 1: Scopeless Lambda Basics

**Goal:** Understand scopeless lambda mechanics

**Code to write:**
```bend
# Test 1: Basic scopeless lambda
def test_basic():
    identity = λ$x $x
    result = identity(42)
    # What is $x here? Should be 42
    return $x

# Test 2: Multiple scopeless bindings
def test_multiple():
    (λ$x λ$y ($x + $y))(10)(20)
    # What are $x and $y? Should be 10 and 20
    return ($x, $y)

# Test 3: Scopeless lambda not called
def test_uncalled():
    uncalled = λ$z $z
    # What is $z? Should be * (ERA - erased)
    return $z

# Test 4: Duplication
def test_dup():
    dup = λ$w $w
    a = dup(1)
    b = dup(2)
    # What is $w? Superposition of 1 and 2?
    return $w
```

**Questions to answer:**
- Do scopeless vars persist across function boundaries?
- How does erasure work?
- What happens with duplication/superposition?
- Can we observe the interaction net graph?

**Success criteria:**
- Understand scoping rules completely
- Predict behavior accurately
- Know edge cases

### Experiment 2: Mutable Reference via Scopeless Lambda

**Goal:** Implement mutable state using scopeless lambdas

**Code to write:**
```bend
# Attempt 1: Simple mutable counter
def mutable_counter():
    # Initialize with scopeless lambda
    init = λ$count 0
    _ = init()  # $count now = 0

    # Increment function
    def increment():
        setter = λ$count ($count + 1)
        setter($count)  # Set $count to $count + 1

    # Get function
    def get():
        return $count

    increment()
    increment()
    return get()  # Should return 2

# Attempt 2: Mutable array/list
def mutable_list():
    init = λ$list []
    _ = init()

    def append(item):
        setter = λ$list ($list ++ [item])
        setter($list)

    def get():
        return $list

    append(1)
    append(2)
    append(3)
    return get()  # Should return [1, 2, 3]
```

**Questions to answer:**
- Does this actually create mutable state?
- Is it safe (no race conditions)?
- What's the performance?
- Does it kill parallelism?

**Success criteria:**
- Working mutable reference
- Understand performance implications
- Know safety guarantees

### Experiment 3: Call/CC Implementation

**Goal:** Implement call-with-current-continuation

**Code to write:**
```bend
# Based on Bend docs mention of call/cc
def callcc(f):
    return λ$k f(k)

# Test 1: Early return
def test_early_return():
    result = callcc(λk =>
        k(42)  # Jump out early with 42
        100    # This should not be reached
    )
    return result  # Should be 42

# Test 2: Save continuation
def test_save_continuation():
    λ$saved_k *  # Initialize saved continuation

    result = callcc(λk =>
        (λ$saved_k k)()  # Save k for later
        10
    )

    # Can we call saved continuation later?
    # saved_k(99)  # Jump back with different value?
    return result

# Test 3: Generator-like behavior
def test_generator():
    λ$yield_k *

    def generator():
        callcc(λk =>
            (λ$yield_k k)()
            1
        )
        callcc(λk =>
            (λ$yield_k k)()
            2
        )
        callcc(λk =>
            (λ$yield_k k)()
            3
        )

    # Can we yield multiple times?
    return generator()
```

**Questions to answer:**
- Does call/cc work as expected?
- Can we save continuations?
- Can we call them multiple times?
- What are the limitations vs Scheme?

**Success criteria:**
- Working call/cc
- Understand continuation lifetime
- Know what's possible/impossible

### Experiment 4: Cooperative Scheduler

**Goal:** Implement yield/resume with continuation-based scheduler

**Code to write:**
```bend
# Scheduler state
λ$task_queue []
λ$current_task *

# Task type: { id, continuation, state }
def create_task(id, f):
    return { id: id, cont: f, state: "ready" }

# Yield: Save continuation and switch to next task
def yield():
    callcc(λk =>
        # Save current continuation
        update_task($current_task, cont: k)
        # Schedule next task
        schedule_next()
    )

# Schedule next task
def schedule_next():
    match $task_queue:
        [] => return "done"
        [task, ...rest] =>
            (λ$task_queue rest)()
            (λ$current_task task)()
            task.cont()  # Resume task

# Spawn task
def spawn(id, f):
    task = create_task(id, f)
    (λ$task_queue ($task_queue ++ [task]))()

# Example tasks
def task1():
    print("Task 1: Step 1")
    yield()
    print("Task 1: Step 2")
    yield()
    print("Task 1: Step 3")

def task2():
    print("Task 2: Step 1")
    yield()
    print("Task 2: Step 2")

# Run scheduler
def main():
    spawn(1, task1)
    spawn(2, task2)
    schedule_next()  # Start scheduler
```

**Questions to answer:**
- Can we implement cooperative multitasking?
- Does interleaving work correctly?
- What's the overhead per context switch?
- How many tasks can we handle?

**Success criteria:**
- Working scheduler with yield/resume
- Tasks interleave correctly
- Measure performance vs manual approach

### Experiment 5: Message Queue

**Goal:** Implement actor-style message queue

**Code to write:**
```bend
# Message queue using mutable list
def create_mailbox():
    λ$mailbox []
    _ = (λ$mailbox [])()

    def send(msg):
        (λ$mailbox ($mailbox ++ [msg]))()

    def receive():
        match $mailbox:
            [] => return None
            [msg, ...rest] =>
                (λ$mailbox rest)()
                return Some(msg)

    return { send: send, receive: receive }

# Test
def test_mailbox():
    mb = create_mailbox()

    mb.send("Hello")
    mb.send("World")

    msg1 = mb.receive()  # Should be Some("Hello")
    msg2 = mb.receive()  # Should be Some("World")
    msg3 = mb.receive()  # Should be None

    return (msg1, msg2, msg3)
```

**Questions to answer:**
- Does FIFO queue work correctly?
- What's the performance?
- Can multiple "actors" have separate mailboxes?
- What about concurrent sends?

**Success criteria:**
- Working message queue
- FIFO ordering preserved
- Isolation between mailboxes

### Experiment 6: Simple Actor

**Goal:** Combine scheduler + mailbox into simple actor

**Code to write:**
```bend
# Actor: { id, state, mailbox, handler }
def create_actor(id, initial_state, handler):
    λ$state initial_state
    λ$mailbox []

    def process_messages():
        match $mailbox:
            [] =>
                yield()  # No messages, yield to other actors
                process_messages()
            [msg, ...rest] =>
                (λ$mailbox rest)()
                new_state = handler($state, msg)
                (λ$state new_state)()
                yield()  # Yield after each message
                process_messages()

    return {
        id: id,
        send: λmsg => (λ$mailbox ($mailbox ++ [msg]))(),
        run: process_messages
    }

# Counter actor
def counter_handler(state, msg):
    match msg:
        Increment => state + 1
        Decrement => state - 1
        Get => state

# Test
def test_actor():
    counter = create_actor(
        id: "counter",
        initial_state: 0,
        handler: counter_handler
    )

    # Spawn actor in scheduler
    spawn("counter", counter.run)

    # Send messages
    counter.send(Increment)
    counter.send(Increment)
    counter.send(Increment)
    counter.send(Decrement)

    # Run scheduler
    schedule_next()

    # How to get result? Need receive channel...
```

**Questions to answer:**
- Can we create actor-like entities?
- Do messages process correctly?
- How to get responses back?
- What's the overhead vs Rust actors?

**Success criteria:**
- Working actor with state + messages
- Correct message processing
- Understand limitations

### Experiment 7: Multiple Actors Communicating

**Goal:** Multiple actors sending messages to each other

**Code to write:**
```bend
# Ping-pong actors
def ping_actor(pong_ref):
    def handler(state, msg):
        match msg:
            Pong =>
                print(f"Ping received Pong #{state}")
                pong_ref.send(Ping)
                state + 1
            Stop =>
                print("Ping stopping")
                state

    create_actor("ping", initial_state: 0, handler)

def pong_actor(ping_ref):
    def handler(state, msg):
        match msg:
            Ping =>
                print(f"Pong received Ping #{state}")
                ping_ref.send(Pong)
                state + 1
            Stop =>
                print("Pong stopping")
                state

    create_actor("pong", initial_state: 0, handler)

# Test
def test_ping_pong():
    # Need circular reference - how to handle?
    λ$ping_ref *
    λ$pong_ref *

    ping = ping_actor($pong_ref)
    pong = pong_actor($ping_ref)

    (λ$ping_ref ping)()
    (λ$pong_ref pong)()

    spawn("ping", ping.run)
    spawn("pong", pong.run)

    ping.send(Pong)  # Start ping-pong

    schedule_next()
```

**Questions to answer:**
- Can actors reference each other?
- Does message passing work between actors?
- How many round-trips before issues?
- What breaks at scale?

**Success criteria:**
- Working actor communication
- Circular references handled
- Understand stability

### Experiment 8: Performance Benchmarking

**Goal:** Compare HVM actors vs Rust actors

**Benchmarks to run:**

1. **Message throughput**
   - Send 1M messages through mailbox
   - Measure time
   - Compare to Rust channel

2. **Actor spawn time**
   - Create 1000 actors
   - Measure time
   - Compare to Rust actor spawn

3. **Context switch overhead**
   - 100 actors, each yielding 1000 times
   - Measure total time
   - Compare to Tokio task switching

4. **Memory usage**
   - 1000 actors with 100 messages each
   - Measure memory
   - Compare to Rust actors

**Success criteria:**
- Quantitative performance data
- Understand bottlenecks
- Know when HVM actors make sense (if ever)

### Experiment 9: Parallelism Interaction

**Goal:** Understand how actor mutability affects HVM parallelism

**Code to write:**
```bend
# Can we have parallel computation AND actors?

# Pure parallel computation
def pure_parallel_work(data):
    # This parallelizes automatically
    map(data, expensive_computation)

# Actor with pure computation
def worker_actor():
    def handler(state, msg):
        match msg:
            Compute(data) =>
                # Does this parallelize?
                result = pure_parallel_work(data)
                sender.send(Result(result))
                state

# Test
def test_parallel_actors():
    # Spawn 10 worker actors
    workers = map(range(10), λi => create_actor(f"worker{i}", 0, handler))

    # Send compute tasks
    for worker in workers:
        worker.send(Compute(large_dataset))

    # Do the pure computations parallelize?
    # Or does actor mutability serialize everything?
```

**Questions to answer:**
- Can actors use HVM's data parallelism?
- Does mutable state serialize everything?
- Hybrid approach possible?
- Best practices for mixing actors + parallelism?

**Success criteria:**
- Understand parallelism constraints
- Know what patterns work
- Design guidelines for Boon

### Experiment 10: Boon Integration Prototype

**Goal:** Design how Boon code should compile to HVM actors

**Boon syntax design:**
```boon
# Option 1: Explicit ACTOR keyword
counter_actor: ACTOR @HVM {
    STATE { count: 0 }

    ON Increment {
        count: count + 1
    }

    ON Decrement {
        count: count - 1
    }

    ON Get REPLY {
        count
    }
}

# Option 2: HOLD as actor (if using HVM backend)
counter: 0 |> HOLD count @HVM {
    LATEST {
        increment_msg |> THEN { count + 1 }
        decrement_msg |> THEN { count - 1 }
    }
}

# Option 3: Explicit process spawn
worker_process: SPAWN @HVM {
    LOOP {
        msg: RECEIVE
        msg |> WHEN {
            Work[data] => BLOCK {
                result: process(data)
                sender |> SEND Result[result]
            }
        }
    }
}
```

**Compilation strategy:**
```
Boon ACTOR/LATEST @HVM
    ↓
Bend actor implementation
    ↓
    ├─→ Scopeless lambda for state
    ├─→ Scopeless lambda for mailbox
    ├─→ Handler function
    └─→ Spawn in scheduler
```

**Questions to answer:**
- What's the best Boon syntax?
- How to handle message types?
- How to handle REPLY pattern?
- Integration with Rust actors?

**Success criteria:**
- Clean Boon syntax
- Clear compilation model
- Type safety maintained

---

## Research Deliverables

### Code Artifacts

1. **`hvm_actors/` library**
   - Scopeless lambda utilities
   - call/cc implementation
   - Scheduler implementation
   - Mailbox/queue primitives
   - Actor creation/spawning
   - Example actors

2. **Benchmarks**
   - Performance comparison suite
   - HVM actors vs Rust actors
   - Scalability tests
   - Memory profiling

3. **Boon integration prototype**
   - Syntax examples
   - Compiler pass (Boon → Bend actors)
   - Type system extensions
   - Integration tests

### Documentation

1. **`HVM_ACTORS_CAPABILITIES.md`**
   - What works
   - What doesn't work
   - Performance characteristics
   - Limitations and edge cases

2. **`HVM_ACTORS_PATTERNS.md`**
   - Design patterns for HVM actors
   - Best practices
   - Anti-patterns to avoid
   - When to use vs Rust actors

3. **`BOON_HVM_INTEGRATION.md`**
   - Compilation strategy
   - Syntax design
   - Type system
   - Runtime integration

4. **Research paper** (optional)
   - "Actor-like Concurrency via Continuations in Interaction Nets"
   - Novel findings
   - Performance analysis
   - Comparison to traditional actors

---

## Timeline & Milestones

### Month 1: Foundation
- ✅ Experiments 1-3 (scopeless lambdas, mutable refs, call/cc)
- 📊 Deliverable: Understanding of HVM primitives

### Month 2: Actor Patterns
- ✅ Experiments 4-6 (scheduler, mailbox, simple actor)
- 📊 Deliverable: Working actor prototype

### Month 3: Scale & Performance
- ✅ Experiments 7-9 (multi-actor, benchmarks, parallelism)
- 📊 Deliverable: Performance analysis

### Month 4: Integration
- ✅ Experiment 10 (Boon integration)
- 📊 Deliverable: Boon syntax and compiler

### Month 5: Documentation
- 📝 Write up findings
- 📊 Deliverable: Complete documentation

---

## Success Metrics

**Research is successful if we:**

1. **Understand the design space**
   - Know what's possible and impossible
   - Understand performance envelope
   - Clear decision criteria for when to use

2. **Have working prototypes**
   - Actor implementation (even if slow)
   - Boon integration design
   - Benchmarks and data

3. **Inform Boon development**
   - Clear strategy for HVM integration
   - Know Rust actors vs HVM actors trade-offs
   - Design patterns documented

4. **Contribute to HVM community**
   - Share findings with HVM authors
   - Potentially influence HVM development
   - Help other researchers

**Success is NOT:**
- Building production actor system
- Matching Rust actor performance
- Replacing existing solutions

**Success IS:**
- Understanding capabilities
- Expanding knowledge
- Making informed decisions

---

## Open Questions for HVM Authors

**Questions to ask Victor Taelin / Higher Order Company:**

1. **Scopeless lambdas:**
   - Are they intended for actor-like patterns?
   - What are the design goals?
   - What are known limitations?

2. **call/cc:**
   - Is full call/cc supported?
   - Can continuations be saved and called multiple times?
   - What's the performance model?

3. **Mutable state:**
   - Is mutable state via scopeless lambdas intended use?
   - Does it break parallelism guarantees?
   - Safety considerations?

4. **HVM3:**
   - What new features are planned?
   - Any actor-related primitives?
   - Better continuation support?

5. **Performance:**
   - Expected overhead for continuation-based patterns?
   - Optimization strategies?
   - Benchmarking best practices?

---

## Related Research

**Papers to read:**

1. **Interaction Combinators**
   - Yves Lafont's original papers
   - Understanding the formal model
   - Relation to lambda calculus

2. **Continuations and Control**
   - Classic call/cc papers
   - Continuation-passing style
   - Delimited continuations

3. **Actor Model**
   - Carl Hewitt's actor papers
   - Erlang/OTP design
   - Akka implementation strategies

4. **Functional Parallelism**
   - Par/seq parallelism
   - Implicit parallelism
   - Interaction nets for parallelism

**Related projects:**

1. **Koka** (algebraic effects)
   - How do effects compare to continuations?
   - Actor implementation via effects?

2. **Esterel/ReactiveML** (synchronous reactive)
   - Different concurrency model
   - Lessons for Boon?

3. **Erlang BEAM** (actor runtime)
   - Benchmarking reference
   - Design patterns to emulate

---

## Risk Mitigation

**What if experiments fail?**

**Scenario 1: Actors don't work well in HVM**
- ✅ Still valuable research (know the limits)
- ✅ Confirms Rust actors are right choice
- ✅ Informs HVM integration strategy

**Scenario 2: Performance is terrible**
- ✅ Understand bottlenecks
- ✅ Know when NOT to use HVM actors
- ✅ Design Boon to avoid anti-patterns

**Scenario 3: Scopeless lambdas don't do what we think**
- ✅ Learn actual semantics
- ✅ Find actual use cases
- ✅ Adjust Boon compilation strategy

**Research has value regardless of outcome!**

---

## Next Steps

1. **Set up HVM/Bend development environment**
   - Install HVM2
   - Learn Bend syntax
   - Run examples

2. **Start with Experiment 1**
   - Understand scopeless lambdas
   - Write basic tests
   - Document findings

3. **Create research repository**
   - `boon-hvm-actors-research/`
   - Track experiments
   - Version control findings

4. **Reach out to HVM community**
   - GitHub discussions
   - Discord/community channels
   - Share research goals

5. **Regular progress updates**
   - Weekly experiment reports
   - Document surprises
   - Iterate on approach

---

## Conclusion

**This is a research project, not a production effort.**

**Goals:**
- ✅ Explore HVM capabilities
- ✅ Understand actor-like patterns
- ✅ Inform Boon/HVM integration
- ✅ Expand knowledge of interaction nets
- ✅ Contribute to HVM community

**Non-goals:**
- ❌ Replace Rust actors
- ❌ Production actor system
- ❌ Match existing performance

**Value:**
- Understanding the design space
- Making informed decisions
- Scientific/academic contribution
- Future-proofing Boon architecture

**Let's explore what's possible! 🔬**

---

**Status:** Research roadmap ready
**Next:** Set up environment and start Experiment 1
**Timeline:** 5 months for comprehensive exploration
**Output:** Knowledge, prototypes, documentation, possibly papers
