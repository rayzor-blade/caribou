<p align="center">
<img style="display: block;" src="assets/caribou.png" alt="Caribou logo" width="250"/>
</p>

<h1 align="center">Caribou</h1>

<p align="center">A shared runtime core for game and multimedia scripting.</p>

<p align="center">
<a href="https://github.com/rayzor-blade/caribou/actions/workflows/ci.yml"><img src="https://github.com/rayzor-blade/caribou/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="https://github.com/rayzor-blade/caribou/actions/workflows/nightly.yml"><img src="https://github.com/rayzor-blade/caribou/actions/workflows/nightly.yml/badge.svg" alt="Nightly"></a>
<a href="https://github.com/rayzor-blade/caribou/actions/workflows/bench.yml"><img src="https://github.com/rayzor-blade/caribou/actions/workflows/bench.yml/badge.svg" alt="Bench"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
</p>

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

## Installing

The nightly `caribou` command, prebuilt for macOS, Linux and Windows:

```sh
curl -fsSL https://caribou.rayzor.tech/install.sh | sh
```

```powershell
irm https://caribou.rayzor.tech/install.ps1 | iex
```

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

## Project Documentation

* `docs/interop.md`: Language interoperability specifications, including namespace mappings, type reflection, explicit export attributes, and marshalling rules.
* `docs/architecture.md`: Architectural documentation for the memory manager, fiber scheduler, FFI call bridge, runtime adapters, and the driver subsystem.
* `docs/building.md`: The workspace crates, the A/B runners, the benchmarks, the wasm lane, the LLVM tier, and CI.
* Issue Tracking: Tracked offline in-tree via `git-bug`. Use `git-bug bug` to query active tasks.

## License

MIT. See [LICENSE](LICENSE).
