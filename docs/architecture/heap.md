# Heap & Memory Management

## Overview

`caribou::heap` is an Immix garbage collector. It is the single managed heap for every guest runtime in the process. The collector never moves objects, so an object's address stays valid for its whole lifetime. It scans memory conservatively by default and precisely where an object's type descriptor asks for it.

The implementation started as a port of Ash's collector. `heap/immix.rs` keeps the order and the names of Ash's `gc.rs` so the two files stay easy to diff. Ash still ships its own copy and does not depend on this crate; Caribou reaches Ash only through the seam described in the [architecture overview](../architecture.md). The type descriptor lives in `heap/desc.rs`.

## Memory Layout

The heap is a single virtual reservation that the collector commits on demand.

* **Reservation Size:** The heap reserves between 512 MB and 4 GB (1 GB on 32-bit targets), or one quarter of physical memory, whichever is smaller. `ASH_GC_HEAP_MB` overrides this limit.
* **Blocks and Lines:** The reservation is divided into 32 KB blocks, and each block into 128-byte lines.
* **Allocation Granularity:** Objects start on 16-byte quanta. A side table stores one byte per quantum and records where each allocation begins; a second table stores the spans per line. Because of these tables, an interior pointer resolves to the object that contains it, and two objects that share a line are marked independently.

Allocation takes one of two paths depending on size:

* **Small allocations** (up to one line) bump through a thread-local buffer and never take the lock.
* **Large allocations** take the heap lock and claim recycled or fresh lines.

Every allocation returns zeroed memory. `allocate_immortal` and `allocate_large` are the only two exceptions to the standard allocation path.

## Allocation Kinds

`alloc_gen(t, size, flags)` is the entry point that HashLink code uses. The low two bits of `flags` select the allocation kind (`caribou_abi::mem::AllocKind`). The collector records the kind in two bits of the allocation's side-table byte, next to its size code and its claim bit.

| Kind | Collector Behavior |
|---|---|
| `Typed` | The collector writes `t` to word zero. The object is scanned conservatively unless `flags` also carries `mem::TRACED`; in that case `t` is a `TypeDesc`, and the object is traced and dropped through the descriptor's hooks. |
| `Raw` | Scanned conservatively. |
| `NoPtr` | Never scanned. |
| `Finalizer` | Scanned conservatively. The collector records the block so that the callback stored in word zero runs once the block becomes unreachable. |

`gc_alloc` and the bump region record no kind. Their allocations are raw.

## Type Descriptors

A `heap::TypeDesc` begins with an exact `hl_type` prefix, so C code that reads word zero sees an ordinary `hl_type*`. The rest of the structure belongs to the core: a trace hook, a drop hook, a name, the defining language, a reload epoch, and an extension pointer. A descriptor is never a heap object itself, and the collector never follows word zero of a traced object.

**Trace Hook:**

* The hook receives the object and a `Tracer`, and marks the objects it references through the tracer.
* `mark(ptr)` claims the allocation that a pointer resolves to, interior pointers included. `mark_value(bits)` does the same for the object encoded in a NaN-boxed `Value`.
* Hooks run inside the stopped world, possibly on a marking thread. They only read their object.
* If a traced object's descriptor has no trace hook, the collector scans the object conservatively past word zero.

**Drop Hook:**

* The hook runs during the sweep, on the collecting thread, for every traced object that the trace did not reach.
* It runs before the collector recycles any line, and it releases the resources the object owns outside the heap.
* After the hook returns, the collector forgets the object's start address. A stale pointer into the object then resolves to nothing, and no later cycle traces or drops the object again.
* A drop hook must not allocate on the heap or take the GC lock. If the hook owns a handle, it releases it through `handle_release_deferred`. Finalizer blocks have a deferred path of their own.

## Root Set

A collection marks from the following roots, in this order:

1. Registered global objects
2. Persistent pins
3. Live handles
4. Root slots: the addresses of pointer slots, re-read on every cycle, which implements HashLink's `hl_add_root` contract
5. The globals array
6. Registered root ranges
7. Each mutator's interpreter scan-root table. The interpreter publishes and maintains this table, and the collector reads its address once
8. Each registered mutator's machine stack, from its saved stack pointer to its stack top, after the callee-saved registers have been spilled
9. Every registered fiber stack, from its saved stack pointer to its top
10. The saved registers of parked mutators

**Handles:** A handle is a reference-counted slot in a table that the GC lock protects (`handle_new`, `handle_get`, `handle_retain`, `handle_release`). Plugins and adapters hold handles across calls instead of raw pointers, because the scanner cannot see a raw pointer stored outside the heap or the registered stacks. `handle_release_deferred` is the release variant for drop hooks, which run inside the collector and cannot take the lock: the release happens the next time the outermost hold of the lock is released, when the queued finalizers run. A null handle is a no-op in every operation.

**Root Ranges:** A root range (`register_root_range`, `unregister_root_range`) is an address range that the collector scans conservatively on every collection. A linked spoke's data section is one example; a module's variable array is another.

**Fiber Stacks:** A fiber stack registers with `gc_register_fiber_stack` and reports its suspended stack pointer with `gc_update_fiber_sp`. The collector skips a stack that has never been suspended. Each stack registers under krio's id, which is unique within the process, so the core's own tasks and a guest runtime's fibers share one registry. WrenLift reports to its seam when a fiber stack is created, suspended, and freed, and the Wren adapter forwards those events to the heap (see [adapters.md](adapters.md#wren-objects)).

## Collection Cycle

A collection stops the world. A mutator enters the rendezvous at a safepoint: every allocation slow path, every blocking primitive, and every point the scheduler polls. Compiled loops reach a safepoint through the poll hook.

**Stop Sequence:**

* When the collector needs the world stopped, it calls the function installed with `set_poll_request_hook`. The scheduler responds by bumping its poll epoch.
* The collector then calls the function installed with `set_stop_hook`. A guest runtime uses this hook to drive its own threads to a safepoint. The collector calls the hook again with `false` once the world is released.

**Marking:** Marking is conservative from the roots. The collector follows every word that resolves to an allocation, except where the allocation's kind says otherwise: it skips a `NoPtr` block, and it walks a traced object through its hook. A small root set marks on one thread; a larger root set marks on a thread pool sized by `ASH_GC_MARK_THREADS`.

**Sweeping:** The sweep drops dead traced objects, frees unmarked lines, and returns wholly free blocks. Finalizers of unreachable blocks are queued and run the next time the lock is released, never inside the collector.

**Triggering:** A collection starts when the bytes allocated since the previous collection, plus the external bytes reported through `track_external`, cross an adaptive threshold. The threshold is twice the live size, clamped between 8 MB and a ceiling that grows with the heap. A heartbeat collects at least every 30 seconds; `CARIBOU_GC_HEARTBEAT_MS` changes the interval. The following switches adjust this behavior:

* `ASH_GC_TRIGGER_MB` fixes the threshold.
* `ASH_GC_STRESS` collects at every opportunity and disables the allocation buffer. It changes the allocation path as well as the collection frequency.
* `ASH_GC_STATS` prints a report at exit.
* The remaining `ASH_GC_*` switches are diagnostic. Each is documented in one line at the point where it is read.

**Deferred Collection:** A trigger that fires inside an allocation normally collects immediately. Some mutators only have a complete root set at points of their own choosing: an interpreter that has published a scan-root table, and a guest collector that has requested deferral with `set_deferred_collection`. For those mutators the trigger records a pending collection instead, which `collect_pending` reads without taking the lock. The collection then runs at the next safepoint any mutator reaches: the interpreter's `scan_roots_done`, an allocation-buffer refill, or the guest collector's own poll. If allocation pressure exceeds four thresholds (or the ceiling, if that is larger), the collector runs inline regardless.

## Hosted Collectors

A guest runtime may keep its own collector on top of this heap; `caribou-wren` does. A collection cycle then consists of both collectors running in sequence.

**Two-Pass Model:**

* The guest collector marks from its own roots and claims everything it marked. `claim_for_cycle` marks an object exactly as the core's marker would, so the core collection that ends the cycle keeps the object.
* Everything the guest collector did not mark becomes *pending*, and the core collection decides its fate. The guest collector knows its own roots, but its objects are also held by the proxy objects other languages keep for them (the protocol's shadow, see [bridge.md](bridge.md#cells--shadows)), by the core's handles, and by frames on stacks the guest collector cannot locate, in particular a core fiber's stack.
* The core's mark reaches all of those holders: a proxy's trace marks the object it represents, every registered stack is scanned, and a pending object that the mark reaches is traced through its descriptor's hook, which clears its pending flag.
* `collect_garbage_then` runs the guest collector between the core's mark and the core's sweep. Whatever is still pending at that point dies in the sweep, before its lines are reused.

The guest collector's marking is a first pass over the objects it knows about. The core's collection makes the final decision about which objects are freed.

**Anchor Objects:** Guest objects survive every other core collection through an anchor object whose trace hook marks all of them, so no core root needs to reach them directly. Wherever the core does reach one of them, it traces the object precisely through the descriptor's hook. An embedder holds a guest object through a handle, which is a root of the core's mark. A language holds one through its proxy object, which needs no root.

**Mutator Registration:** The guest runtime's thread is an ordinary mutator. It registers at the OS stack top and runs in deferred mode, so a collection that any other mutator starts waits for this thread to park, and the core's trigger never collects inside this thread's allocations. The thread's `should_collect` checks the combined condition: the core's trigger is due, or a collection is pending, or the heap has allocated a full threshold since the thread's own last cycle (this prevents other mutators, whose collections reset the shared trigger, from starving it), or the heartbeat has elapsed with allocation outstanding. The thread answers a stop request in the same poll by parking (`gc_safepoint`), never by starting a cycle of its own. If it started a cycle, that cycle would stop the world as well, and two guest heaps would end up collecting each other in a loop. `collect_begin` re-enters the rendezvous before it takes the lock.

**Write Consistency Invariant:** One invariant keeps the other thread's precise trace sound. The guest thread parks only at a safepoint (its poll, its allocation, or any slot that takes the GC lock), and its runtime completes every write to an object between two such points. As a result, no object is mid-write while the thread is parked.

**Multi-Threaded Guest Runtimes:** A guest runtime with threads of its own registers each one as a mutator in the same way, for as long as the thread executes the runtime's code. The runtime's own world already knows when a thread is safe (in a wait or a native call), and that state maps directly onto the core's blocking region:

* `gc_block_at(sp, extra)` is the entry for a runtime that has already published the thread's stack pointer. The second argument is an extra range covering the registers the runtime saved when it stopped a compiled loop.
* `gc_unblock` holds the thread while a collection is in progress.
* A thread the runtime has not stopped is executing compiled code that the core cannot reach. The stop hook exists for this case: the guest runtime has its own way of stopping such a thread, and the hook asks it to do so. Every thread goes through the same safe and running transitions no matter which collector asked.

**Bidirectional Rendezvous:** The same applies in the other direction. A thread that is executing the core's own code, or another language's, reaches the guest collector's rendezvous at the core's safepoints. The guest runtime signals when it asks its threads to stop (`hosted_stop`). While such a stop is in progress, every `gc_safepoint` runs the safepoint hook, where the adapter makes the thread's view safe and holds the thread. A thread inside a core blocking region is safe for the guest collector as well, because the blocking hook runs at the outermost entry and exit of the region. Both collectors share one rendezvous, reachable from either side.

## Locking & Thread Registration

* **Allocator Lock:** One reentrant lock protects the allocator. `gc_locked_init` initializes the singleton on first use and returns a guard. The lock is held for every structural change and released around finalizers. Releasing the outermost hold drains both deferred queues (handle releases and finalizers) until they are empty, because a finalizer may itself release a handle.
* **Thread Registration:** `register_thread` and `set_stack_top` make a thread's stack visible to the collector. An unregistered thread must not hold heap pointers across a safepoint.

## Current Implementation Boundaries

* Only `alloc_gen` records an allocation kind. The bump region's allocations are raw, so a traced object always takes the locked allocation path.
* A `Typed` allocation is traced only when its caller declares that `t` is a `TypeDesc`. Ash's bare `hl_type` values remain conservative.
* The persistent pin set is stored beside the handle table and is not reference-counted.
* There is one heap per process, as HashLink requires.
