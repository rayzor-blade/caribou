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

`caribou::sched` is Ash's fiber scheduler redesigned over krio-core's
`Task`. `sched/world.rs` is the per-thread world and its loop, `task.rs`
the task kinds and host state, `wait.rs` the wait tokens and parking,
`preempt.rs` the poll epoch and its timer, `pool.rs` the worker pool.

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
`Suspension` that comes back. `spawn_fiber(stack_size, body)` makes a
stackful task on the calling world, registering its stack with the heap and
charging it as external pressure until the task is dropped; `spawn(task)`
takes any `Task`; `spawn_fiber_on_pool` places a stackful task on the
least-loaded worker world at spawn time. The default stack is 256 KB.

### The scheduler loop

Per world, the scheduler holds a ready queue of task ids, a timer heap
keyed by deadline, and the tasks themselves. A turn resumes every task that
was ready when the turn began; tasks parked on a token or a timer consume no
switch. The main context, the thread's original stack, drives turns when it
blocks or when the driver ticks the world; a task never drives a turn, it
yields.

Host state is a `HostState` object an adapter attaches to a task, or to the
main context under `TaskId::NONE`, with `attach_host_state`: Ash's trap
chain and pending exception, Zyntax's effect handler stack. Around each
resume the scheduler swaps the main context's state out, the task's in,
steps the task, publishes the task's suspended stack pointer to the heap,
runs the world's switch hook, then swaps the task's state out and the main
context's back in. The switch hook (`set_switch_hook`, one per world) runs
only after the stack pointer is published, because a hook that publishes
interpreter roots may honour a pending collection. A task's record stays in
the world while it runs; only its body is taken out, so a running task can
attach state to itself.

### Parking

`park(waiter, deadline)` is the one blocking primitive. A waiter is a wait
token; `wake(token)` marks it notified and moves the task to the ready
queue. On a task, park records the request and yields; on the main context,
park drives scheduler turns and the reactor until notified or timed out; on
a thread the runtime did not create, park polls the token with a short
sleep, because such a thread has no fiber to yield and may not run tasks.
Locks, semaphores, conditions, deques and sleeps are all built on park and
wake. A task that parks with a deadline is also on the timer heap; whichever
fires first wins and the other is cancelled. A stackless task cannot yield
from inside `park`; it calls `request_park` and returns `Pending`, and reads
`resume_cause` when next stepped. Whether a thread drives or polls is
decided by `has_world()`: a thread that has spawned or ticked owns a world.

### The reactor

Not yet built. Today, when no task is ready, the main context blocks in
`scheduler_idle` on the world's endpoint until a command arrives from
another world or the next timer is due. The reactor will be the world's
source of external wakeups beyond that: socket readiness, file watches,
channels from OS threads. Blocking I/O in any language will register with
it and park; the reactor wakes the token. The seam is marked in
`world.rs`.

### Preemption and safepoints

Compiled loops poll one word, `POLL_EPOCH` (exported as the symbol
`caribou_poll_epoch`; `poll_epoch_address` hands code generators its
address), on every back-edge. A timer thread bumps it every two
milliseconds while any task exists; the collector's stop request bumps it
through the heap's poll hook, which the first world installs. A task that
observes a changed epoch calls `poll`: a heap safepoint, then a yield on a
task or one turn on the main context. So no task can starve the others and
the world can always be stopped. The interpreter and every blocking
primitive are safepoints as well. `enter_blocking` and `leave_blocking`
mark a task as outside the heap's reach for the duration of a native call.

### Multiple worlds

A process may run several worlds on several OS threads over the one heap.
Tasks are pinned to the world that created them; a krio fiber is `!Send`
and never migrates. Ash's worker pool for compiled thread bodies is the
first use: it chooses a world at spawn time and never moves the task
afterwards. Worlds exchange `Wake` and `Spawn` commands through per-world
endpoints. The pool is sized by `CARIBOU_WORKERS`, or `ASH_WORKERS`, or the
machine; on wasm there is no pool and no timer thread, and `yield_now`
routes through krio's host suspender. Collections stop every world at its
safepoints. `CARIBOU_SCHED_TRACE` prints every switch and park; safe.

### Adapter contract

An adapter provides: a way to build a task from its own callable (a Haxe
closure, a Wren fiber object, a Zyntax function), rooting that callable
itself; the per-task host state the scheduler swaps; and a switch hook if it
keeps interpreter roots to publish. It consumes: `spawn`, `spawn_fiber`,
`park`, `wake`, `yield_now`, `sleep_until`, `poll`, `current_task`, and
`tick(deadline)` for a driver that owns the frame loop. Ash's rule that a
new thread runs to its first blocking point before `thread_create` returns
is the adapter's to keep, with one `schedule_step` after spawning.

### Boundaries of the current implementation

- No reactor: idle blocks on the endpoint and the timer heap only.
- Heap fiber-stack ids are `u32` and task ids `u64`; the id is truncated.
- The main stack's published probe sits above the callee-saved registers
  krio spills at a switch, as in Ash.

## World

A world is what a driver holds: the handle through which it registers the
runtimes it uses, loads code, calls into it, ticks the scheduler and
subscribes to events. One world is one OS thread, one scheduler, one view
of the process-wide heap. `caribou::world` is the registry of adapters and
their languages; it does not own the heap or the scheduler, which are
per process and per thread respectively, but it is the only path a driver
uses to reach them.

### Adapters and languages

An adapter is a runtime that has been taught the core: it implements
`Adapter`, registering with `World::register`. Registration hands the
adapter the `LangId`s it owns: one for a single-language runtime, one per
grammar snapshot for Zyntax. A `LangId` names a namespace in the module
registry (`lang:path`), the `lang` field of every `TypeDesc` the adapter
creates, and the language a module section in a bundle belongs to.

`Adapter` supplies: its language names; `load(source: ModuleSource) ->
ModuleId`, taking bytecode, source text or a blob by the adapter's own
format; `lookup(module, name) -> Option<Callable>`; `call(callable, args:
&[Value]) -> Result<Value, Error>` through the bridge; `reload(module)`
returning a plan for the registry to apply; and the per-task `HostState`
it wants attached when the world spawns a task on its behalf.

### Startup and the frame loop

`World::new(config)` initialises the heap if needed and the calling thread's
scheduler, and installs the heap's poll hook. The driver then registers
adapters, loads modules, and either calls `World::run_main(module)` to let
the world own the loop, or calls `World::tick(deadline)` from its own frame
loop. `tick` runs scheduler turns, drains reload checks and delivers events
until the deadline. Ash's frame pump is the first driver: the world ticks
inside it, between the Haxe application's frames.

### Events

`World::on(kind, handler)` subscribes a handler to `Reload`, `TaskError`
and `Log` events. Handlers run on the world's thread, from `tick`, never
from inside a collection or a switch.

### Boundaries of the current implementation

Not yet built beyond the adapter registry and the language table. Module
loading, lookup, call and events arrive with the bridge and the module
registry.
