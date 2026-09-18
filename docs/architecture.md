# Caribou architecture

Caribou is the runtime core under Ash (Haxe, on HashLink bytecode),
WrenLift (Wren) and, later, Zyntax (DSLs, and the Lua and Python front
ends built on it). It owns what those runtimes used to own separately:
the heap, the fiber scheduler, the module registry, the call bridge
between languages, and the native plugin boundary. Each runtime is an
adapter over it. A Haxe application drives a world; the other languages
are spokes it loads.

These pages describe the systems as built. Design rationale lives in the
design proposal, and work in progress lives in git-bug.

## The pages

| Page | What it covers |
|---|---|
| [Heap](architecture/heap.md) | The Immix collector: memory, allocation kinds, type descriptors, roots, collection, hosted collectors, locking. |
| [Scheduler](architecture/scheduler.md) | Worlds and tasks, the loop, parking, preemption, multiple worlds, what an adapter provides. |
| [Bridge](architecture/bridge.md) | Values at the boundary, the object protocol, symbols, errors, typed dispatch, calls, call sites, guards, cells and shadows, diagnostics. |
| [Adapters](architecture/adapters.md) | How a Haxe object and a Wren object answer the protocol, calling into Haxe under a trap or a guard, Wren dispatch, fibers and runs, functions crossing either way. |
| [Registry](architecture/registry.md) | Interfaces and namespaces, and what Ash and WrenLift publish. |
| [Wren imports a Haxe class](architecture/wren-imports.md) | Resolving and installing, a call, lifetime. |
| [Haxe imports a Wren class](architecture/haxe-imports.md) | The build macro, declaring types, the emitted class, binding the natives, cells, how a Wren object is held by Haxe. |
| [World and driver](architecture/world.md) | Adapters and languages, startup, the driver and its project layout, the run report, events. |
| [Linking](architecture/linking.md) | A member at link time: the symbol, the C signature, each AOT's half, what stays at run time. |
| [Bundle](architecture/bundle.md) | A program and its modules in one file: what is in it, building, opening. |
| [Plugins](architecture/plugins.md) | Native code on the shared ABI: writing one, loading, a plugin as a language of its own. |

[interop.md](interop.md) is the companion for a program's author: what a
program writes and what it can expect.

## Hosting a runtime

No runtime depends on caribou. Each runtime keeps its own collector,
scheduler and build, and exposes a **seam**: a table of the entry points
its own code reaches its runtime through, filled with its own
implementations by default. When caribou hosts that runtime, it installs
its implementations into the table before the runtime allocates. From
then on the runtime's heap and fibers are the core's. The runtime's tests
run against its own implementations; caribou's adapter tests run the same
runtime against the core's.

The seam is a runtime act, not a link-time one. Nothing in a runtime's
manifest names caribou. A caribou adapter crate depends on the runtime,
loads it, and fills the table. Ash's seam is `ash_std::rt`. The pattern is
the one Ash already used for its closure runner and switch hook: an atomic
slot per entry, a `hlp_set_*`-style installer, and the runtime's own
function as the fallback.

A seam covers exactly what the core replaces: allocation and collection,
thread and fiber-stack registration with the collector, safepoints and
blocking, the scheduler's spawn, park, wake and step, and the poll epoch's
address. It does not cover what the runtime keeps whoever hosts it:
object layouts, type tables, closures, exceptions, native-call
marshaling.

## Crates

| Crate | Role |
|---|---|
| `caribou_abi` | `no_std`, zero dependencies. The layouts and constants shared by the core, every adapter and every plugin: HashLink's `hl.h` structs with size and offset tests, the NaN-boxed `Value`, allocation kinds, the plugin descriptor table, error kinds. It never defines a symbol. |
| `caribou` | The core. Depends on `caribou_abi`, `ariadne` for rendering diagnostics, `libc` on unix, `windows-sys` on Windows. |
| `caribou-ash` | Ash's adapter. `install()` fills `ash_std::rt` with the core's heap and scheduler. With the `runner` feature, `program` loads a `.hl` on ash's interpreter over the core, runs it and publishes its classes to the registry, and the `caribou-ash` binary runs one, or with `--no-install` on ash's own runtime for A/B. Depends on ash by path until ash is published; builds on nightly, as ash_std does. |
| `caribou-wren` | WrenLift's adapter. `install()` fills `wren_lift::runtime::rt`, the memory under its Immix strategy, with the core's heap. The `caribou-wren` binary (feature `runner`) runs a `.wren` on WrenLift's interpreter or tiered JIT over the core, or with `--no-install` on WrenLift's own heap for A/B. Depends on wren_lift by path until it is published. |
| `caribou-driver` | The `Session` an embedder opens and runs a program through, and the `caribou` command (`run`, `describe`) as a thin front over it. |
| `caribou-interop` | Tests and the benchmarks: both adapters in one process, values crossing between them, and the two-language projects under `fixtures/`. |
