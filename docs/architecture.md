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

## Hosting a runtime

No runtime depends on caribou. Each runtime keeps its own collector,
scheduler and build, and exposes a **seam**: a table of the entry points
its own code reaches its runtime through, filled with its own
implementations by default. Caribou, when it hosts that runtime, installs
its implementations into the table before the runtime allocates, so the
runtime's heap and fibers become the core's. The runtime's tests run
against its own implementations; caribou's adapter tests run the same
runtime against the core's.

The seam is a runtime act, not a link-time one. Nothing in a runtime's
manifest names caribou. A caribou adapter crate depends on the runtime,
loads it, and fills the table. Ash's seam is `ash_std::rt`; the pattern is
the one Ash already used for its closure runner and switch hook: an atomic
slot per entry, a `hlp_set_*`-style installer, the runtime's own function
as the fallback.

What a seam covers is exactly what the core replaces: allocation and
collection, thread and fiber-stack registration with the collector,
safepoints and blocking, the scheduler's spawn, park, wake and step, and
the poll epoch's address. What it does not cover is anything the runtime
keeps whatever hosts it: object layouts, type tables, closures, exceptions,
native-call marshaling.

## Crates

| Crate | Role |
|---|---|
| `caribou_abi` | `no_std`, zero dependencies. Layouts and constants shared by the core, every adapter and every plugin: HashLink's `hl.h` structs with size and offset tests, the NaN-boxed `Value`, allocation kinds, the plugin descriptor table, error kinds. It never defines a symbol. |
| `caribou` | The core. Depends on `caribou_abi`, `ariadne` for rendering diagnostics, `libc` on unix, `windows-sys` on Windows. Stable Rust. |
| `caribou-ash` | Ash's adapter: `install()` fills `ash_std::rt` with the core's heap and scheduler, and the `caribou-ash` binary (feature `runner`) runs a `.hl` on ash's interpreter over the core, or with `--no-install` on ash's own runtime for A/B. Depends on ash by path until ash is published; builds on nightly, as ash_std does. |
| `caribou-wren` | WrenLift's adapter: `install()` fills `wren_lift::runtime::rt`, the memory under its Immix strategy, with the core's heap, and the `caribou-wren` binary (feature `runner`) runs a `.wren` on WrenLift's interpreter or tiered JIT over the core, or with `--no-install` on WrenLift's own heap for A/B. Depends on wren_lift by path until it is published; stable Rust. |
| `caribou-interop` | Tests only: both adapters in one process, values crossing between them through the bridge. Nightly, as caribou-ash is. |

## Heap

`caribou::heap` is an Immix collector: non-moving, conservative by default,
precise where an object's descriptor asks for it. It began as a port of
Ash's collector; `heap/immix.rs` keeps the order and names of Ash's `gc.rs`
so the two stay diffable. Ash keeps its own copy and does not depend on
this crate: caribou reaches Ash's runtime through the seam described under
"Hosting a runtime". `heap/desc.rs` is the type descriptor.

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
with the heap. A heartbeat collects at least every 30 seconds
(`CARIBOU_GC_HEARTBEAT_MS` overrides the interval). `ASH_GC_TRIGGER_MB`
fixes the threshold; `ASH_GC_STRESS` collects at every opportunity and
disables the allocation buffer, which changes the allocation path as well as
the frequency; `ASH_GC_STATS` prints a report at exit. The remaining
`ASH_GC_*` switches are diagnostic and documented one line each where they
are read.

A trigger that fires inside an allocation collects there unless the
allocating mutator's roots are complete only at a point of its own: an
interpreter that publishes its scan-root table has this property once it
registers the table, and a hosted collector asks for it with
`set_deferred_collection`. For such a mutator the trigger records a pending
collection instead, readable without the lock through `collect_pending`, and
the collection runs at the next safepoint any mutator reaches: the
interpreter's `scan_roots_done`, an allocation-buffer refill, or the hosted
collector's own poll. Pressure past four thresholds, or past the ceiling if
that is more, collects inline regardless.

### Hosted collectors

A runtime that keeps its own collector over this heap (caribou-wren does)
runs its cycle on the core's per-cycle claim: `claim_for_cycle` marks what
its trace reaches, it drops and forgets what was not claimed, and it ends
with a core collection whose sweep clears the claims. Its objects are kept
alive across every core collection by an anchor object whose trace hook
marks them all, so no core root needs to reach them, and they are traced
precisely through the descriptor's hook wherever the core reaches them.

The runtime's thread is an ordinary mutator, registered at the OS's stack
top and put in deferred mode, so a collection any other mutator starts waits
for it to park, and the core's trigger never collects inside its allocation.
Its `should_collect` is the mutual condition: the core's trigger is due, a
collection is pending, the heap alone has allocated a threshold's worth
since its own last cycle (so another mutator's collections, which reset the
shared trigger, cannot starve it), or the heartbeat has elapsed with
something allocated. A stop request is answered in the same poll by parking
(`gc_safepoint`), not by a cycle: a cycle would stop the world in turn, and
two hosted heaps would collect each other without end. `collect_begin`
enters the rendezvous again before taking the lock. The invariant that makes
the other thread's precise trace sound: the hosted thread parks only at a
safepoint (its poll, its allocation, any slot that takes the GC lock), and
its runtime completes every write to an object between two of those, so no
object is mid-write while parked.

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
`tick(deadline)` for a driver that owns the frame loop; `has_worker_pool`,
`is_pool_worker` and `any_live_tasks` answer the placement and blocking
questions Ash's primitives ask before they spawn or wait. Ash's rule that a
new thread runs to its first blocking point before `thread_create` returns
is the adapter's to keep, with one `schedule_step` after spawning.

### Boundaries of the current implementation

- No reactor: idle blocks on the endpoint and the timer heap only.
- Heap fiber-stack ids are `u32` and task ids `u64`; the id is truncated.
- The main stack's published probe sits above the callee-saved registers
  krio spills at a switch, as in Ash.

## Object protocol and bridge

`caribou::protocol` is the message set, `caribou::symbol` the names,
`caribou::error` the error value, `caribou::bridge` the call, and
`caribou::diag` the report a driver prints. Each adapter supplies its half:
the protocol for its own objects, and for Ash the typed dispatcher
(see "Adapters" below).

### Values at the boundary

Two ABIs meet at every cross-language call. Typed HashLink code passes raw
scalars and pointers by signature; every dynamically typed runtime passes
`caribou_abi::Value`, one NaN-boxed word. The core converts between them
once, at the edge, and never inside a language.

An object `Value` that reaches the bridge has a `TypeDesc` at word zero.
That is the protocol's contract: an adapter whose native objects carry a
bare `hl_type` wraps them before they cross.

### The protocol

Every heap object answers a closed set of messages through the `Protocol`
vtable its `TypeDesc` points at. An adapter implements the vtable once for
its own types; the core dispatches through it when a value crosses into a
language that did not make it.

| Message | Meaning |
|---|---|
| `get_member`, `set_member` | a named field or property, by interned symbol |
| `invoke` | call a named member with arguments |
| `call` | call the object itself, if callable |
| `index`, `set_index`, `len`, `iterate` | sequence and map access |
| `to_string`, `hash`, `equals` | identity and display |
| `unwrap_native` | the native payload of a plugin object |
| `is_error`, `error_message`, `error_kind`, `error_cause`, `error_trace` | the error protocol |

A message an object does not answer returns `Unsupported`; the calling
language maps that to its own notion of a missing member. An entry that
raises sets the error pending on the current task and answers `Raised`.
Entries are `extern "C-unwind"`, so a panic inside a Rust entry travels to
the bridge's protected boundary instead of aborting inside the entry.

### Symbols

`caribou::symbol` is one interner per process behind a lock. `intern`
gives a `Symbol` for a name, `name` gives the name back, and the strings
are leaked because a symbol lives as long as the process. Each symbol also
carries `hash`, HashLink's field hash of its name: the same loop Ash's
`hlp_hash_gen` runs, over UTF-16 code units, `h = 223 * h + unit` in
wrapping 32-bit arithmetic and then a truncating remainder by
`0x1FFFFF7B`. So a name hashes here to what `hashed_name` holds for it in
Ash. HashLink's own table additionally probes upward when two live names
collide; that depends on its cache and is not reproduced.

### Errors

`Error` is a heap object of the core's own language, `LANG_CORE`: a
`KIND_DYNAMIC | TRACED` allocation under a static `TypeDesc` whose trace
hook marks its fields and whose protocol answers the five error messages,
`to_string` and `get_member` for `kind`, `message`, `cause`, `native`,
`origin` and `trace`. Its fields are the kind from `caribou_abi::ErrorKind`,
the language that raised it, a message, an optional cause, an optional
native payload and a trace. The message is a core `Str`, a UTF-8 string
whose bytes follow a two-word header; it is traced with no children, so the
bytes are never scanned. The trace is a core `Trace`, a fixed-capacity list
of frames that is replaced by a larger copy when it fills. A frame records
the language crossed, the callable's name, and when known the source it was
in (a file path or module name, as a core `Str`) and a byte span into that
source. `push_frame` records a frame with a source and span; `push_segment`
records one with neither, which is what the bridge does at a boundary.

The native payload is the originating language's own error object, kept so
that a round trip unwraps to it. `with_native` builds a `User` error around
one; `from_value` tells an `Error` from any other value by its descriptor.

Nothing in this module is boxed: every object is reached through a raw
pointer or a `Value`. A NaN-boxed `Value` on the stack is invisible to the
conservative scanner, so the module roots every object it creates by a
handle from allocation until it is stored in a rooted parent or returned,
and roots every object it holds across an allocation.

### The pending error

An error leaves a language as a pending value, not as an unwinding
exception. `bridge::set_pending` stores it for the current task,
`take_pending` returns it and clears the slot, `has_pending` asks. The slot
is a thread-local map keyed by task id rather than a `HostState`: a task has
one host-state slot and it belongs to the adapter that spawned it, and a
task never leaves the world that created it, so this thread's map holds
exactly one slot per task of this world. The slot roots its value through a
handle while it waits, since the map is not a heap root. A task that
finishes without its error being taken leaves an entry the next insert past
a small threshold sweeps away.

### Typed dispatch

A typed callable is a C function pointer with an `hl_type_fun`-shaped
signature and the language it belongs to. The bridge does not marshal such a
call itself: each language registers a `TypedDispatch` with
`set_typed_dispatch`, a C-ABI function that takes the function, the
signature, the arguments as `Value`s and an out slot, and answers a reply
code. Ash registers one that goes through its own `hlp_dyn_call`, under its
own id. The core registers a default for `LANG_CORE`
that covers what the core's own callables and the tests need: up to four
arguments of kinds `HI32`, `HBOOL`, `HF64` and `HDYN`, returning `HVOID`,
`HI32`, `HBOOL`, `HF64` or `HDYN`, by transmuting the function to the C type
those classes describe. It relies on integer-class arguments of any width
sharing a register or slot, which holds on the native ABIs the core runs on
and not on wasm.

### Calls

`bridge::call(callable, args, caller)` invokes a callable on behalf of the
language `caller`. A dynamic callable is sent `call`; a typed one is checked
for arity against its signature and handed to its language's dispatcher.
`call_named` does the same with the callee's name for the trace;
`invoke`, `get` and `set` send `invoke`, `get_member` and `set_member` with
the same handling. Every crossing guarantees three things.

It is protected: the dispatch runs under `catch_unwind`, and a panic becomes
an `Internal` error carrying the panic's message. An entry that answered
`Unsupported` or `Missing` becomes a `Type` or `Runtime` error naming the
value and the member. An entry that answered `Raised` yields the pending
error; a pending value that is not an `Error` is wrapped in one as its
native payload, so it still unwraps at home.

Every error leaving a call carries one more trace frame: the callee's
language, its name if the caller gave one and `<callable>` otherwise, and no
source.

A value returning to the language that raised it is unwrapped: when the
error's native payload is set and its origin is `caller`, the call returns
the payload rather than the `Error`, so a Haxe exception that passed through
Wren and back is the same Haxe object it was.

### Diagnostics

`caribou::diag` prints an error in two steps, the shape wren_lift and
zyntax already use. `report(err)` turns an `Error` value into a plain
`Diagnostic`: the kind and message, one label per frame that has a span
(its language, name, source id and byte span), the frames without a span as
notes in order, a note for a native payload, and the cause chain as nested
diagnostics. No ariadne type appears in it, so an adapter can feed it to its
own renderer, or hand the core a diagnostic of its own. `render(diag,
sources, out, color)` draws one with ariadne: the kind and message as the
header, each label on its source line coloured by language from a fixed
palette, the notes, then each cause as a further report whose message
begins `caused by:`. `sources` is a `SourceLookup`, one method from a
source id to its text, implemented over a map in tests and over module
tables in adapters; a label whose source cannot be found is written as a
note. `render_string` returns the same as a string. Language names come
from the process-wide table `World::register` fills, `world::language_name`.

### Adapters

Each adapter's `Runtime` implements `world::Adapter` with one language,
`haxe` or `wren`. `World::register` hands it an id; the adapter writes the
id into the descriptor its objects carry, and Ash also registers its typed
dispatcher under it. The order is fixed: both seams (`caribou_ash::install`,
`caribou_wren::install`) before either runtime allocates, then the world
and its registrations, then the first VM. Registration comes before the VM
because the id it writes is read from the descriptor by every message and
every collection from then on.

A Haxe object crosses wrapped. Its word zero is a bare `hl_type`, which has
no protocol slot, so `caribou_ash::wrap` makes a `HaxeRef`: a two-word core
object under a static descriptor whose trace hook marks the object and
whose protocol reaches it through ash's own dynamic access. `get_member`
and `set_member` go through `hlp_dyn_getp` and `hlp_dyn_setp`, by the
field hash `hlp_hash_gen` gives the symbol's name, computed once per
symbol. `invoke` finds the method on the runtime's lookup chain and calls
it through the typed dispatcher with the object as `this`. `call` runs a
closure through `hlp_dyn_call`, and `to_string` is `hlp_value_to_string`.
`equals` and `hash` are the wrapped object's identity, so two wrappers of
one object compare equal and no cache is kept. `unwrap` gives the object
back. Sequence access is not answered yet.

Ash's dispatcher builds a `vclosure` without a bound value around the code
pointer and its signature, boxes each argument by the signature's kind into
the `vdynamic` `hlp_dyn_call` takes (an object argument is the wrapped
object itself), and unboxes the result by the return kind. Every call into
Haxe code runs under a HashLink trap whose setjmp frame is a C function of
the adapter's own (`trap.c`), armed with `hlp_setup_trap_jit`, so a
`hl_throw` inside lands there instead of unwinding through Rust. The thrown
value becomes a core `Error`: a bytes value is the runtime's own error and
its kind is read from the message (`Null access`, out of bounds, divide by
zero), a String or any other object is a `User` error; the exception itself
is the error's native payload, wrapped, so it is the same object when it
returns to Haxe.

A Wren object crosses as its own core address: the start of the prefixed
allocation, where word zero is the shared `WREN_DESC`, sixteen bytes before
the address wren_lift holds. `caribou_wren::wrap` and `unwrap` translate,
and every protocol entry adds the prefix back before touching the object.
A core int becomes a Wren number on the way in, Wren having no other, and
a core string becomes a Wren string; an object of another language cannot
enter Wren yet. The protocol answers through the runtime's own methods by
Wren's signature convention: `get_member` is the getter `name`, else an
instance field of that name; `set_member` is `name=(_)`; `invoke` is
`name(_,_)` by arity, falling back to the getter for an arity of zero;
`call` runs a closure; `index`, `set_index`, `len` and `iterate` are `[_]`,
`[_]=(_)`, `count` and `iterate(_)` with `iteratorValue(_)`. A method the
class lacks raises Wren's own `does not implement` error. wren_lift's error
is a message string, so a Wren error crossing the bridge is a core `Error`
whose native payload is that message as a Wren string, rooted by a VM
handle; no Wren object is an error by itself.

The Wren entries run on a VM, and `install` cannot know it. Whoever creates
a VM enters it: `enter_vm` and `leave_vm` around a run, or `with_vm` around
a call that may reach Wren objects; the runner enters its VM for the whole
run. An entry with no VM entered falls back to the one wren_lift reports as
dispatching, and raises an `Internal` error if there is none.

## Worlds and tasks

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
`tick(deadline)` for a driver that owns the frame loop; `has_worker_pool`,
`is_pool_worker` and `any_live_tasks` answer the placement and blocking
questions Ash's primitives ask before they spawn or wait. Ash's rule that a
new thread runs to its first blocking point before `thread_create` returns
is the adapter's to keep, with one `schedule_step` after spawning.

### Boundaries of the current implementation

- No reactor: idle blocks on the endpoint and the timer heap only.
- Heap fiber-stack ids are `u32` and task ids `u64`; the id is truncated.
- The main stack's published probe sits above the callee-saved registers
  krio spills at a switch, as in Ash.

## Object protocol and bridge

Not yet built beyond the types. This section is the contract phase 3 is
written against.

### Values at the boundary

Two ABIs meet at every cross-language call. Typed HashLink code passes raw
scalars and pointers by signature; every dynamically typed runtime passes
`caribou_abi::Value`, one NaN-boxed word. The core converts between them
once, at the edge, and never inside a language.

### The protocol

Every heap object answers a closed set of messages through the `Protocol`
vtable its `TypeDesc` points at. An adapter implements the vtable once for
its own types; the core dispatches through it when a value crosses into a
language that did not make it.

| Message | Meaning |
|---|---|
| `get_member`, `set_member` | a named field or property, by interned symbol |
| `invoke` | call a named member with arguments |
| `call` | call the object itself, if callable |
| `index`, `set_index`, `len`, `iterate` | sequence and map access |
| `to_string`, `hash`, `equals` | identity and display |
| `unwrap_native` | the native payload of a plugin object |
| `is_error`, `error_message`, `error_kind`, `error_cause`, `error_trace` | the error protocol |

A message an object does not answer returns `Unsupported`; the calling
language maps that to its own notion of a missing member.

### Callables

`Callable` is what the bridge invokes: a typed function (a C pointer plus an
`hl_type_fun`-shaped signature) or a dynamic one (a `Value` that answers
`call`). `bridge::call(callable, args) -> Result<Value, Error>` marshals
dynamic values into a typed call by the signature, or passes them through
to a dynamic one, and always returns through a protected boundary: an
error leaving the callee's language becomes an `Error` value here and is
re-raised natively by whoever receives it.

Ash's typed dispatcher and its reflection trampoline stay Ash's; the Ash
adapter registers them as the typed half of the bridge through the seam.

### Errors

`Error` is a heap value with a `TypeDesc` of the core's own language: a
kind from `caribou_abi::ErrorKind`, a message, an optional cause, an
optional native payload the originating language keeps its own error in,
and a trace assembled one segment per boundary crossed. A value returning
to the language that raised it is unwrapped to the original object.

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
