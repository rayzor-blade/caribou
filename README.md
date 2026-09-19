<p align="center">
<img style="display: block;" src="assets/caribou.png" alt="Caribou logo" width="250"/>
</p>

<h1 align="center">Caribou</h1>

<p align="center">A shared runtime core for game and multimedia scripting.</p>

# Caribou Multi-Language Runtime Core

Caribou is a shared execution runtime for multi-language environments, serving three guest runtimes:

* [Ash](https://github.com/rayzor-blade/ash?utm_source=gemini) (Haxe on HashLink bytecode)
* [WrenLift](https://github.com/wrenlift/WrenLift?utm_source=gemini) (Wren)
* Zyntax (DSLs, with upcoming Lua and Python frontends)

Caribou consolidates four runtime systems into a unified implementation:

* Global managed heap
* Cooperative task and fiber scheduler
* Cross-language module and namespace registry
* Unified native dynamic plugin loader

This architecture allows seamless multi-language execution within a single process. A game engine written primarily in Haxe can drive gameplay systems scripted in Wren, pass native objects across language boundaries without serializing, and hot-reload scripts at runtime.

## How It Works

Guest runtimes maintain zero compile-time dependencies on Caribou. Ash and WrenLift retain independent garbage collectors, fiber schedulers, test suites, and build scripts; their project manifests do not reference Caribou.

Integration is handled at runtime via dynamic dispatch seams:

* **The Seam Table:** Each guest runtime exposes a struct of function pointers covering all heap allocations, collections, and scheduling actions. By default, these point to the runtime's native implementations.
* **Adapter Initialization:** Dedicated adapter crates intercept and overwrite these function pointer tables with Caribou implementations before any runtime memory allocation occurs.
* **Isolated Testing:** Standalone test suites execute against native implementations unmodified, while adapter builds execute identical guest binaries over the unified Caribou core.

In typical deployments, the host application (typically Haxe) controls the process entry point, primary loop, and final executable distribution. The host loads secondary languages as guest scripts that can be edited and hot-reloaded during execution in development and production builds without rebuilding native Rust components.

## Workspace Crates

| Crate | Target / Toolchain | Description |
| --- | --- | --- |
| `caribou_abi` | Stable (`no_std`) | ABI contract with zero external dependencies. Defines HashLink `hl.h` struct layouts (with static offset assertions), NaN-boxed `Value` definitions, allocation tags, error variants, and the `plugin!` macro. |
| `caribou` | Stable | Runtime engine containing the shared heap, scheduler, object protocol, and root `World` execution state. Memory uses an Immix-based non-moving collector (conservative by default, precise when guided by type descriptors). The scheduler manages stackful fibers and stackless state machines within a unified run queue. Depends on `caribou_abi`, `krio`, and `libc` (no LLVM or Cranelift dependencies). |
| `caribou-ash` | Nightly | Ash runtime adapter. Overwrites `ash_std` seam hooks with Caribou heap and scheduler bindings. |
| `caribou-wren` | Stable | WrenLift adapter. Overwrites `wren_lift` seam hooks with Caribou's Immix-backed memory allocator. |
| `caribou-plugin` | Stable | Plugin loader that registers native shared libraries as runtime languages using typed FFI dispatchers over C signatures. |
| `caribou-zyntax` | Stable | Zyntax host adapter. Registers frontends (snapshots, `.zyn` grammars, or a frontend with its own parser) as guest languages, compiles modules via the embed runtime, and publishes what each frontend's own conventions export. |
| `caribou-python` | Stable | The Python frontend of Zyntax (`zyntax_python`) as a guest language: registered when a root holds a `.py` file, with Python's module layout and export rules. |
| `caribou-driver` | Nightly | Host execution supervisor. Discovers workspace files, initializes the `World`, loads guest languages, and backs the `caribou` CLI. |

## Implementation Status

Ash and WrenLift are operational atop the Caribou core and validated against their native baselines:

* **Ash:** Matches native execution parity across the Ash test corpus and the upstream Haxe language conformance suite.
* **WrenLift:** Matches standalone benchmark timings and memory consumption under high GC pressure.

### Interoperability & Cross-Imports

Modules are resolved and shared across language boundaries through a unified namespace:

* **Wren importing Haxe:** `import "game:Player" for Player` resolves to Haxe classes.
* **Haxe importing Wren:** Using `-lib caribou`, `import game.hud.Hud` imports `src/game/hud.wren`. Exported Wren method signatures can be defined explicitly (e.g., `#export = "add(n: Num) -> Num"`) or inferred automatically.
* **Native Plugins:** C libraries built with `caribou_abi::plugin!` in `plugins/` are treated as first-class languages in the module registry.
* **Zyntax:** Discovered frontends (ZynML snapshots, `.zyn` files, or Python sources) register beside standard Wren and Haxe modules. A module's functions belong to the module: Wren imports them as module variables; Haxe sees the module's classes under the module as their package.

## Building & Verification

### Build Commands

```sh
# Core runtime
cargo build -p caribou
cargo test -p caribou

# Language runners & CLI
cargo +nightly build -p caribou-ash --features runner
cargo build -p caribou-wren --features runner
cargo +nightly build -p caribou-driver

```

### Build Requirements & Workspace Layout

* Runtimes are pinned to specific revisions in `Cargo.toml`.
* Ash is patched to a local checkout at `../ash` and requires building `ash_std` first (`cargo build -p ash_std`) due to embedded library dependencies.
* `caribou-ash` requires the `LLVM_SYS_211_PREFIX` environment variable during compilation, as Ash's build script expects this configuration even when building without LLVM linking.
* Active Zyntax branches may be patched against local checkouts at `../zyntax`.
* Local Haxe integration: Register the library locally via `haxelib dev caribou haxe` to enable `-lib caribou`.

### Running Programs

Execute a HashLink binary directly from a project root:

```sh
caribou run bin/game.hl

```

Package the primary executable, related classpath modules, and native plugins into a unified archive:

```sh
caribou build bin/game.hl    # Emits bin/game.cb
caribou run bin/game.cb      # Executes self-contained bundle

```

Append `--report` to inspect execution diagnostics upon exit:

```sh
caribou run --report bin/game.hl

```

The report outputs JIT tier promotions, boxed value allocations, and bridge call paths (direct vs. dynamic fallback).

### Standalone A/B Testing

Adapter runner binaries provide a `--no-install` flag to bypass Caribou seam patching, running code directly against the guest runtime's native subsystems for performance and parity comparisons:

```sh
target/debug/caribou-ash --mode hybrid game.hl
target/debug/caribou-wren --mode tiered script.wren

```

### Benchmarks

Run integration benchmarks through `caribou-interop`:

```sh
# Compare cross-bridge invocation overhead against intra-language calls
cargo bench -p caribou-interop --bench interop

# Benchmark frame times across pure Haxe, pure Wren, and hybrid splits
cargo bench -p caribou-interop --bench swarm

# Profile cross-boundary memory allocations and collection impact
cargo bench -p caribou-interop --bench transfer

```

### JIT Tiers & LLVM Configuration

WrenLift executes on Cranelift by default. To enable the LLVM optimizing tier, build `caribou-wren`, `caribou-driver`, or `caribou-interop` with `--features llvm`. This requires an LLVM 21 installation and links LLVM dynamically across all JIT components (including Ash).

## Project Documentation

* `docs/interop.md`: Language interoperability specifications, including namespace mappings, type reflection, explicit export attributes, and marshalling rules.
* `docs/architecture.md`: Architectural documentation for the memory manager, fiber scheduler, FFI call bridge, runtime adapters, and the driver subsystem.
* Issue Tracking: Tracked offline in-tree via `git-bug`. Use `git-bug bug` to query active tasks.

## License

MIT. See [LICENSE](LICENSE).
