# Caribou architecture

Caribou is the runtime core under Ash (Haxe via HashLink bytecode), WrenLift
(Wren) and Zyntax (DSLs, and the Lua and Python front-ends built on it). It
owns what those runtimes used to own separately: the heap, the fiber
scheduler, the module registry with hot reload, the call bridge between
languages, and the native plugin boundary. Each runtime is an adapter over
it. A Haxe application is the default driver of a world; the other languages
are spokes it loads.

This document describes the systems as built. Design rationale lives in the
design proposal; work in progress lives in git-bug.

## Crates

| Crate | Role |
|---|---|
| `caribou_abi` | `no_std`, zero dependencies. Layouts and constants shared by the core, every adapter and every plugin: HashLink's `hl.h` structs with size and offset tests, the NaN-boxed `Value`, allocation kinds, the plugin descriptor table, error kinds. It never defines a symbol. |
| `caribou` | The core. Depends on `caribou_abi`, `libc` on unix, `windows-sys` on Windows. Stable Rust. |

## Heap

`caribou::heap` is an Immix collector: non-moving, conservative by default,
precise where an object's descriptor asks for it. It is Ash's collector moved
here; `heap/immix.rs` keeps the order and names of Ash's `gc.rs` so the two
stay diffable, and Ash re-exports the entry points as `extern "C"` forwarders
under their HashLink names. `heap/desc.rs` is the type descriptor.

### Memory

The heap is one virtual reservation, committed on demand, sized between
512 MB and 4 GB (1 GB on 32-bit targets) or a quarter of physical memory,
whichever is smaller; `ASH_GC_HEAP_MB` overrides. It is divided into 32 KB
blocks of 128-byte lines. Objects are placed at 16-byte quanta; a per-quantum
side table records where each allocation starts and a per-line table records
spans, so an interior pointer resolves to its object and two objects sharing
a line are marked independently.

Small allocations (up to one line) bump through a thread-local allocation
buffer. Larger ones take the lock and claim recycled or fresh lines. Every
allocation returns zeroed memory. `allocate_immortal` and `allocate_large`
are the two exceptions to the ordinary path.

### Allocation kinds

`alloc_gen(t, size, flags)` is the HashLink-facing entry: the low two bits of
`flags` select the kind (`caribou_abi::mem::AllocKind`), and the kind is
recorded in two bits of the allocation's side-table byte, beside its size
code and claim bit.

| Kind | Behaviour |
|---|---|
| `Typed` | `t` is written to word zero. Scanned conservatively, unless `flags` also carries `mem::TRACED`: then `t` is a `TypeDesc` and the object is traced and dropped through its hooks. |
| `Raw` | Scanned conservatively. |
| `NoPtr` | Never scanned. |
| `Finalizer` | Scanned conservatively; recorded so the callback the caller stores in word zero runs once the block is unreachable. |

`gc_alloc` and the bump region record no kind: their allocations are raw.

### Type descriptors

`heap::TypeDesc` begins with exactly an `hl_type`, so C code reading word
zero sees an `hl_type*`; the tail is the core's: a trace hook, a drop hook,
a name, the defining language, a reload epoch and an extension pointer. A
descriptor is never a heap object and the collector never follows word zero
of a traced object.

The trace hook receives the object and a `Tracer`, and marks through it:
`mark(ptr)` claims the allocation a pointer resolves to, interior pointers
included; `mark_value(bits)` does the same for the object payload of a
NaN-boxed `Value`. Hooks run inside the stopped world, possibly on a marking
thread, and only read their object. A traced object whose descriptor has no
trace hook is scanned conservatively past word zero.

The drop hook runs at sweep, on the collecting thread inside the stopped
world, for every traced object the trace did not reach, before any line is
recycled: it releases what the object owns outside the heap. The object's
start is then forgotten, so a stale pointer into it resolves to nothing and
no later cycle traces or drops it again. A drop hook must not allocate on the
heap or take the GC lock. Finalizer blocks keep their own deferred path.

### Roots

A collection marks from, in order: registered global objects, persistent
pins, live handles, root slots (addresses of pointer slots, re-read every
cycle, which is HashLink's `hl_add_root` contract), the globals array,
registered root ranges, each mutator's interpreter scan-root table (published
live, so the interpreter maintains it and the collector reads the address
once), each registered mutator's machine stack from its saved stack pointer
to its stack top with callee-saved registers spilled first, every registered
fiber stack from its saved stack pointer to its top, and the saved registers
of parked mutators.

A handle (`handle_new`, `handle_get`, `handle_retain`, `handle_release`) is a
counted slot in a table under the GC lock: what a plugin or adapter holds
across calls instead of a raw pointer the scanner cannot see. A root range
(`register_root_range`, `unregister_root_range`) is an address range scanned
conservatively at every collection: a linked spoke's data section, a module's
variable array.

Fiber stacks register with `gc_register_fiber_stack` and report their
suspended stack pointer with `gc_update_fiber_sp`; a stack that was never
suspended is skipped.

### Collection

Collections are stop-the-world. A mutator enters the rendezvous at a
safepoint: every allocation slow path, the blocking primitives, and whatever
the scheduler polls. Compiled loops reach a safepoint through the poll hook:
the collector calls the function installed with `set_poll_request_hook` when
it needs the world stopped, and the scheduler bumps its epoch in response.

Marking is conservative from the roots and follows every word that resolves
to an allocation, except through objects whose kind says otherwise: a
`NoPtr` block is skipped and a traced object is walked by its hook. It runs
on one thread for small root sets and on a pool otherwise;
`ASH_GC_MARK_THREADS` sets the pool size. Sweeping drops dead traced
objects, frees unmarked lines and returns wholly free blocks; finalizers of
unreachable blocks are queued and run when the lock is next released, never
inside the collector.

A collection is triggered when bytes allocated since the last one, plus
external bytes reported through `track_external`, cross an adaptive
threshold: twice the live size, clamped between 8 MB and a ceiling that grows
with the heap. A heartbeat collects at least every 30 seconds.
`ASH_GC_TRIGGER_MB` fixes the threshold; `ASH_GC_STRESS` collects at every
opportunity and disables the allocation buffer, which changes the allocation
path as well as the frequency; `ASH_GC_STATS` prints a report at exit. The
remaining `ASH_GC_*` switches are diagnostic and documented one line each
where they are read.

### Locking

One reentrant lock guards the allocator. `gc_locked_init` initialises the
singleton on first use and hands back a guard; the lock is held for every
structural change and released around finalizers. Thread registration
(`register_thread`, `set_stack_top`) is what makes a thread's stack visible;
an unregistered thread may not hold heap pointers across a safepoint.

### Boundaries of the current implementation

- Only `alloc_gen` records a kind. The bump region's allocations are raw,
  so a traced object always takes the locked allocation path.
- A `Typed` allocation is traced only when its caller says `t` is a
  `TypeDesc`; Ash's bare `hl_type`s stay conservative.
- The persistent pin set is kept beside the handle table, uncounted.
- There is one heap per process, as HashLink requires.

## Scheduler

Not yet built. This section is the design it is built to; it becomes a
description once the code lands.

### Worlds and tasks

A world is one OS thread with one scheduler and one reactor. Every language
runs its concurrency on the world's scheduler: a Haxe `sys.thread.Thread`,
a Wren `Fiber`, a Zyntax `fiber def` are all handles to scheduler tasks.
Cross-language calls are synchronous calls on the current task, so a call
chain through three languages suspends and resumes as one unit.

The unit of scheduling is krio-core's `Task`, not the fiber. Two kinds
exist:

- A **stackful task** owns a krio fiber with its own machine stack. It
  suspends from any call depth by switching stacks. Ash's threads, Wren's
  fibers, and Zyntax's `fiber def` are stackful.
- A **stackless task** is a compiled state machine whose `step` runs to its
  next suspension point and returns. Zyntax's `async` and resumable effects,
  and WrenLift's action-loop and AOT-transformed fibers, are stackless. On
  wasm, where the host cannot switch stacks, every task is stackless or is
  driven by the host's suspension.

The scheduler does not distinguish them: it calls `step` and reads the
`Suspension` that comes back.

### The scheduler loop

Per world, the scheduler holds a ready queue of task ids, a timer heap
keyed by deadline, and the tasks themselves. A turn resumes every task that
was ready when the turn began; tasks parked on a token or a timer consume no
switch. The main context, the thread's original stack, drives turns when it
blocks or when the driver ticks the world; a task never drives a turn, it
yields.

Before resuming a task the scheduler records the main stack's suspended
pointer for the collector and swaps the task's host state into the thread's
live cells: Ash's trap chain and pending exception, Zyntax's effect handler
stack, whatever an adapter registers per task. After the task yields the
scheduler publishes the task's suspended stack pointer to the collector
before anything else can observe the switch, then swaps the host state back.
The adapter hook that observes switches runs only after the stack pointer is
published.

### Parking

`park(waiter, deadline)` is the one blocking primitive. A waiter is a wait
token; `wake(token)` marks it notified and moves the task to the ready
queue. On a task, park records the request and yields; on the main context,
park drives scheduler turns and the reactor until notified or timed out; on
a thread the runtime did not create, park polls the token with a short
sleep, because such a thread has no fiber to yield and may not run tasks.
Locks, semaphores, conditions, deques and sleeps are all built on park and
wake. A task that parks with a deadline is also on the timer heap; whichever
fires first wins and the other is cancelled.

### The reactor

The reactor is the world's source of external wakeups: timers, socket
readiness, file watches, and channels from other worlds and from OS threads.
When no task is ready the main context blocks in the reactor until the next
timer or event, instead of sleeping on a fixed cadence. Blocking I/O in any
language registers with the reactor and parks; the reactor wakes the token.

### Preemption and safepoints

Compiled loops poll one word, the poll epoch, on every back-edge. A timer
thread bumps it every two milliseconds while more than one task exists; the
collector's stop request bumps it through the heap's poll hook. A task that
observes a changed epoch reaches a safepoint and yields, so no task can
starve the others and the world can always be stopped. The interpreter and
every blocking primitive are safepoints as well.

### Multiple worlds

A process may run several worlds on several OS threads over the one heap.
Tasks are pinned to the world that created them; a krio fiber is `!Send`
and never migrates. Ash's worker pool for compiled thread bodies is the
first use: it chooses a world at spawn time and never moves the task
afterwards. Collections stop every world at its safepoints.

### Adapter contract

An adapter provides: a way to build a task from its own callable (a Haxe
closure, a Wren fiber object, a Zyntax function), the per-task host state
the scheduler swaps, and a switch hook if it keeps interpreter roots to
publish. It consumes: `spawn`, `park`, `wake`, `yield_now`, `sleep_until`,
`current_task`, the reactor's registration calls, and `tick(deadline)` for a
driver that owns the frame loop.
