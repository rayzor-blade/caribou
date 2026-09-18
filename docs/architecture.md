# Caribou Architecture & Runtime Core

## Overview

Caribou is the shared execution core underpinning Ash (Haxe targeting HashLink bytecode), WrenLift (Wren), and Zyntax (domain-specific languages, along with the Lua and Python frontends built upon it). It consolidates critical runtime subsystems previously maintained independently across each environment:

* Unified managed heap
* Fiber and task scheduler
* Global module and namespace registry
* Cross-language call bridge and FFI
* Native plugin boundary

Each supported language runtime acts as an adapter layer over this core. In typical topology, a Haxe host application drives the world environment, dynamically loading and coordinating the remaining runtimes as dependent guest modules.

## Scope & Documentation

This documentation specifies the current implementation and system architecture as built. For architectural decision records and conceptual background, refer to the design proposals; active tasks and tracked issues are maintained in `git-bug`.

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
| [Zyntax](architecture/zyntax.md) | Zyntax frontends as languages: snapshots and grammars under the roots, modules published from the typed AST and the HIR. |

[interop.md](interop.md) is the companion for a program's author: what a
program writes and what it can expect.

## Runtime Virtualization & The Seam Model

Caribou decouples the core engine from guest language runtimes using dynamic dispatch seams. Runtimes maintain zero compile-time dependencies on Caribou; each environment retains its own standalone garbage collector, task scheduler, and independent build pipeline.

Interception occurs at runtime rather than link-time:

* **The Seam Interface:** A runtime exposes a function pointer table covering core execution hooks, populated by default with its native standalone implementations.
* **Core Interception:** During initialization—prior to any heap allocation—the host adapter overwrites these table entries with Caribou implementations. Once patched, the runtime delegates heap operations and fiber scheduling directly to Caribou.
* **Testing Isolation:** Standalone test suites execute against the runtime's native subsystems. Adapter integration tests run the identical runtime binaries patched against Caribou.

For example, Ash exposes its seam through `ash_std::rt`. This interface uses atomic function pointer slots paired with configuration setters (following the `hlp_set_*` convention), falling back to standalone routines if left unconfigured.

**Subsystem Boundaries:**

* **Intercepted Subsystems (Replaced by Core):**
* Memory allocation, deallocation, and GC collection passes
* Thread and fiber stack registration with the core garbage collector
* Safepoint coordination and blocking operations
* Fiber scheduling primitives: `spawn`, `park`, `wake`, and `step`
* Poll epoch address resolution


* **Retained Subsystems (Managed by Guest Runtime):**
* Language-specific object memory layouts
* Type registries and metadata tables
* Closure invocation and lifecycle mechanics
* Exception handling and stack unwinding
* Native call ABI parameter marshalling



## Crate Architecture

| Crate | Responsibilities & Dependencies |
| --- | --- |
| `caribou_abi` | Header-equivalent contract (`no_std`, zero dependencies). Exports core memory layouts, HashLink `hl.h` struct mirrors with layout/offset assertions, NaN-boxed `Value` definitions, allocation kinds, plugin descriptor tables, and error codes. Declares no concrete symbols. |
| `caribou` | Central runtime engine. Implements the shared heap, GC, fiber scheduler, and FFI bridge. Depends on `caribou_abi`, `ariadne` (diagnostics), and platform primitives (`libc` on UNIX, `windows-sys` on Windows). |
| `caribou-ash` | Ash (HashLink/Haxe) runtime adapter. `install()` intercepts `ash_std::rt`. Under the `runner` feature, loads and executes `.hl` bytecode via Ash's interpreter atop the Caribou core, registering emitted classes with the module registry. The CLI provides a `--no-install` flag for comparative A/B profiling against standalone Ash. |
| `caribou-wren` | WrenLift adapter. `install()` patches `wren_lift::runtime::rt`, binding Wren's Immix-backed memory interface to the Caribou heap. Under the `runner` feature, executes `.wren` source using either the interpreter or tiered JIT over the core, with `--no-install` support for A/B benchmarking. |
| `caribou-driver` | Embedder interface. Exposes the top-level execution `Session` and drives the `caribou` command-line tool (providing subcommands such as `run` and `describe`). |
| `caribou-interop` | Integration testing and performance test suites. Executes concurrent dual-adapter environments in a single process, validating cross-language memory transfer and orchestrating multi-language workspace tests in `fixtures/`. |