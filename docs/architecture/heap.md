# Heap

`caribou::heap` is an Immix collector. It does not move objects. It scans
conservatively by default, and precisely where an object's descriptor asks
for it.

It began as a port of Ash's collector. `heap/immix.rs` keeps the order and
the names of Ash's `gc.rs`, so the two stay easy to diff. Ash keeps its own
copy and does not depend on this crate; caribou reaches Ash through the
seam described in the [overview](../architecture.md). `heap/desc.rs` holds
the type descriptor.

## Memory

The heap is one virtual reservation, committed on demand. Its size is
between 512 MB and 4 GB (1 GB on 32-bit targets), or a quarter of physical
memory, whichever is smaller. `ASH_GC_HEAP_MB` overrides that. The
reservation is divided into 32 KB blocks, and each block into 128-byte
lines.

Objects sit on 16-byte quanta. A side table with one byte per quantum
records where each allocation starts, and a table per line records spans.
So an interior pointer resolves to its object, and two objects sharing a
line are marked independently.

Small allocations, up to one line, bump through a thread-local buffer.
Larger ones take the lock and claim recycled or fresh lines. Every
allocation comes back zeroed. `allocate_immortal` and `allocate_large` are
the two exceptions to the ordinary path.

## Allocation kinds

`alloc_gen(t, size, flags)` is the entry HashLink code uses. The low two
bits of `flags` select the kind, `caribou_abi::mem::AllocKind`. The kind is
recorded in two bits of the allocation's side-table byte, next to its size
code and its claim bit.

| Kind | What the collector does with it |
|---|---|
| `Typed` | `t` is written to word zero. Scanned conservatively, unless `flags` also carries `mem::TRACED`: then `t` is a `TypeDesc`, and the object is traced and dropped through its hooks. |
| `Raw` | Scanned conservatively. |
| `NoPtr` | Never scanned. |
| `Finalizer` | Scanned conservatively. Recorded, so the callback the caller stores in word zero runs once the block is unreachable. |

`gc_alloc` and the bump region record no kind. Their allocations are raw.

## Type descriptors

A `heap::TypeDesc` begins with exactly an `hl_type`, so C code that reads
word zero sees an `hl_type*`. The tail belongs to the core: a trace hook, a
drop hook, a name, the defining language, a reload epoch and an extension
pointer. A descriptor is never itself a heap object, and the collector
never follows word zero of a traced object.

The trace hook receives the object and a `Tracer`, and marks through it.
`mark(ptr)` claims the allocation a pointer resolves to, interior pointers
included. `mark_value(bits)` does the same for the object inside a
NaN-boxed `Value`. Hooks run inside the stopped world, possibly on a
marking thread, and only read their object. A traced object whose
descriptor has no trace hook is scanned conservatively past word zero.

The drop hook runs at sweep, on the collecting thread, for every traced
object the trace did not reach. It runs before any line is recycled, and it
releases what the object owns outside the heap. After it the object's start
is forgotten: a stale pointer into it resolves to nothing, and no later
cycle traces or drops it again. A drop hook must not allocate on the heap
or take the GC lock. If it owns a handle, it gives it up through
`handle_release_deferred`. Finalizer blocks have their own deferred path.

## Roots

A collection marks from these, in this order:

1. registered global objects;
2. persistent pins;
3. live handles;
4. root slots: addresses of pointer slots, re-read every cycle, which is
   HashLink's `hl_add_root` contract;
5. the globals array;
6. registered root ranges;
7. each mutator's interpreter scan-root table, published live, so the
   interpreter maintains it and the collector reads its address once;
8. each registered mutator's machine stack, from its saved stack pointer
   to its stack top, with the callee-saved registers spilled first;
9. every registered fiber stack, from its saved stack pointer to its top;
10. the saved registers of parked mutators.

A handle is a counted slot in a table under the GC lock: `handle_new`,
`handle_get`, `handle_retain`, `handle_release`. It is what a plugin or an
adapter holds across calls instead of a raw pointer the scanner cannot see.
`handle_release_deferred` is the release for a drop hook, which runs inside
the collector and cannot take the lock: the handle goes at the next
outermost release of the lock, when the queued finalizers run. A null
handle is a no-op everywhere.

A root range (`register_root_range`, `unregister_root_range`) is an address
range scanned conservatively at every collection. A linked spoke's data
section is one; a module's variable array is another.

Fiber stacks register with `gc_register_fiber_stack` and report their
suspended stack pointer with `gc_update_fiber_sp`. A stack that was never
suspended is skipped. A stack is registered under krio's id for it, which
is unique in the process, so the core's own tasks and a hosted runtime's
fibers share one registry: wren_lift tells its seam's stack slots when a
fiber's stack is made, suspended and freed, and the Wren adapter forwards
them (see [adapters.md](adapters.md#wren-objects)).

## Collection

Collections stop the world. A mutator enters the rendezvous at a safepoint:
every allocation slow path, the blocking primitives, and whatever the
scheduler polls. Compiled loops reach a safepoint through the poll hook.
When the collector needs the world stopped, it calls the function installed
with `set_poll_request_hook`, and the scheduler bumps its epoch in answer;
then the one installed with `set_stop_hook`, by which a hosted runtime
drives its own threads to a safepoint, and calls it again with `false`
once the world is released.

Marking is conservative from the roots. It follows every word that resolves
to an allocation, except through objects whose kind says otherwise: a
`NoPtr` block is skipped, and a traced object is walked by its hook. A
small root set marks on one thread; a larger one on a pool, sized by
`ASH_GC_MARK_THREADS`. Sweeping drops dead traced objects, frees unmarked
lines and returns wholly free blocks. Finalizers of unreachable blocks are
queued, and run when the lock is next released, never inside the
collector.

A collection is triggered when the bytes allocated since the last one, plus
the external bytes reported through `track_external`, cross an adaptive
threshold: twice the live size, clamped between 8 MB and a ceiling that
grows with the heap. A heartbeat collects at least every 30 seconds;
`CARIBOU_GC_HEARTBEAT_MS` changes the interval. `ASH_GC_TRIGGER_MB` fixes
the threshold. `ASH_GC_STRESS` collects at every opportunity and disables
the allocation buffer, which changes the allocation path as well as the
frequency. `ASH_GC_STATS` prints a report at exit. The other `ASH_GC_*`
switches are diagnostic, and each is documented in one line where it is
read.

Usually a trigger that fires inside an allocation collects right there.
Some mutators have complete roots only at points of their own: an
interpreter that publishes a scan-root table, once it has registered it,
and a hosted collector that asks with `set_deferred_collection`. For those
the trigger records a pending collection instead, readable without the
lock through `collect_pending`. The collection then runs at the next
safepoint any mutator reaches: the interpreter's `scan_roots_done`, an
allocation-buffer refill, or the hosted collector's own poll. Pressure past
four thresholds, or past the ceiling if that is more, collects inline
regardless.

## Hosted collectors

A runtime may keep its own collector over this heap; caribou-wren does. A
cycle is the two collectors in turn. The hosted collector marks from its
own roots, and claims what it marked: `claim_for_cycle` marks an object
exactly as the core's marker would, so the core collection the cycle ends
with retains it. What it did not mark is dead, and it drops and forgets it
before that collection — unless another language may still hold it. An
object another language keeps a stand-in on (the protocol's shadow, see
[bridge.md](bridge.md#shadows)), and everything it reaches, is left to the
core: the stand-in is a core object, and its trace marks the object it
stands for, which the core then traces through the descriptor's hook. The
core's mark decides; `collect_garbage_then` runs the hosted collector
between the mark and the sweep, and `is_claimed_start` tells it which of
those objects the mark reached. The rest die there, before their lines
return.

Its objects stay alive across every other core collection through an
anchor object, whose trace hook marks them all, so no core root needs to
reach them. Wherever the core does reach one, the object is traced
precisely through the descriptor's hook.

The core's handles are roots of the hosted cycle too. `for_each_handle`
gives the addresses live handles root. The hosted collector marks those of
its objects among them, and everything they reach, before it decides what
is dead. That is how an embedder holds a hosted object: by a handle. A
language holds one by its stand-in, which needs none.

The runtime's thread is an ordinary mutator. It registers at the OS's
stack top and runs in deferred mode, so a collection any other mutator
starts waits for it to park, and the core's trigger never collects inside
its allocation. Its `should_collect` is the mutual condition: the core's
trigger is due, or a collection is pending, or the heap alone has allocated
a threshold's worth since its own last cycle (so other mutators'
collections, which reset the shared trigger, cannot starve it), or the
heartbeat has elapsed with something allocated. A stop request is answered
in the same poll by parking (`gc_safepoint`), not by a cycle. A cycle would
stop the world in turn, and two hosted heaps would collect each other
without end. `collect_begin` enters the rendezvous again before taking the
lock.

One invariant makes the other thread's precise trace sound. The hosted
thread parks only at a safepoint, which is its poll, its allocation, or any
slot that takes the GC lock, and its runtime completes every write to an
object between two of those. So no object is mid-write while the thread is
parked.

A hosted runtime with threads of its own makes each a mutator the same
way, for as long as it runs the runtime's code. Its own world already
knows when a thread is safe, in a wait or a native call, and that maps
onto the core's blocking region: `gc_block_at(sp, extra)` is the entry a
runtime that has already published the thread's stack pointer uses, with
a second range for the registers it saved when it stopped a compiled
loop, and `gc_unblock` holds the thread while a collection is under way.
A thread the runtime has not stopped is running compiled code, and the
core cannot reach it; that is what the stop hook is for. The hosted
runtime has its own way of stopping such a thread, and the hook asks it
to use it, so every thread passes through the same safe and running
transitions whichever collector asked. The two worlds are then one
rendezvous with two ways in.

## Locking

One reentrant lock guards the allocator. `gc_locked_init` initialises the
singleton on first use and hands back a guard. The lock is held for every
structural change, and released around finalizers. Releasing the outermost
hold drains both deferred queues, handle releases and finalizers, until
they are empty, since a finalizer may give up a handle.

Thread registration (`register_thread`, `set_stack_top`) is what makes a
thread's stack visible. An unregistered thread may not hold heap pointers
across a safepoint.

## Boundaries of the current implementation

- Only `alloc_gen` records a kind. The bump region's allocations are raw,
  so a traced object always takes the locked allocation path.
- A `Typed` allocation is traced only when its caller says `t` is a
  `TypeDesc`. Ash's bare `hl_type`s stay conservative.
- The persistent pin set is kept beside the handle table, uncounted.
- There is one heap per process, as HashLink requires.
