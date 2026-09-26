# Building & Verification

What the README leaves out: the crates, the comparison runners, the benchmarks, the wasm lane, the LLVM tier, and what CI runs. The build commands and requirements are in the [README](../README.md).

## Workspace Crates

| Crate | Target / Toolchain | Description |
| --- | --- | --- |
| `caribou_abi` | Stable (`no_std`) | ABI contract with zero external dependencies. Defines HashLink `hl.h` struct layouts, NaN-boxed `Value` definitions, allocation tags, error variants, and the `plugin!` macro. |
| `caribou` | Stable | Runtime engine containing the shared heap, scheduler, object protocol, and root `World` execution state. Memory uses an Immix-based non-moving collector. Depends on `caribou_abi`, `krio`, and `libc` (no LLVM or Cranelift dependencies). |
| `caribou-ash` | Nightly | Ash runtime adapter. Overwrites `ash_std` seam hooks with Caribou heap and scheduler bindings. |
| `caribou-wren` | Stable | WrenLift adapter. Overwrites `wren_lift` seam hooks with Caribou's Immix-backed memory allocator. |
| `caribou-plugin` | Stable | Plugin loader that registers native shared libraries as runtime languages using typed FFI dispatchers over C signatures. |
| `caribou-zyntax` | Stable | Zyntax host adapter. Registers frontends (snapshots, `.zyn` grammars, or a frontend with its own parser) as guest languages, compiles modules via the embed runtime, and publishes what each frontend's own conventions export. |
| `caribou-python` | Stable | The Python frontend of Zyntax (`zyntax_python`) as a guest language: registered when a root holds a `.py` file, with Python's module layout and export rules. |
| `caribou-runtime` | Nightly | What an AOT or wasm program runs on: `ash_std`, WrenLift's runtime and the core with both adapters, in one static library. A static constructor fills both seams before the program's entry point creates the heap. |
| `caribou-driver` | Nightly | Host execution supervisor. Discovers workspace files, initializes the `World`, loads guest languages, and backs the `caribou` CLI. |

## Standalone A/B Testing

Adapter runner binaries provide a `--no-install` flag to bypass Caribou seam patching, running code directly against the guest runtime's native subsystems for performance and parity comparisons:

```sh
target/debug/caribou-ash --mode hybrid game.hl
target/debug/caribou-wren --mode tiered script.wren

```

## Benchmarks

Run integration benchmarks through `caribou-interop`:

```sh
# Compare cross-bridge invocation overhead against intra-language calls
cargo bench -p caribou-interop --bench interop

# Benchmark frame times across pure Haxe, pure Wren, and hybrid splits
cargo bench -p caribou-interop --bench swarm

# Profile cross-boundary memory allocations and collection impact
cargo bench -p caribou-interop --bench transfer

```

## The Core on wasm32

The core compiles for `wasm32-unknown-unknown` and `wasm32-wasip1`, and its unit tests run there. The lane uses Ash's wasm host (`ash-wasm-run`, a wasmtime host built from `../ash`) as the runner:

```sh
cargo test -p caribou --target wasm32-wasip1 --no-run     # prints the .wasm paths
ash-wasm-run target/wasm32-wasip1/debug/build/caribou/*/out/caribou-*.wasm --test-threads=1
```

Tests that need a second thread or unwinding are marked ignored on wasm; a wasm module has one thread and aborts on panic. The scheduler's integration tests stay off wasm until fibers there are host-driven.

## The wasm Runtime Object

A program built ahead of time for wasm links against one prelinked runtime object, as Ash's own wasm builds do. Caribou's is the `caribou-runtime` crate, and `caribou build --target wasm32-wasip1 game.hl` builds the program and links it in one command.

The driver makes the object when it is built with its `llvm` feature. Its build script builds `caribou-runtime` for `wasm32-wasip1` in a target directory of its own, then joins it with WASI's libc and `libsetjmp`. It finds a WASI sysroot (`WASI_SYSROOT`, wasi-libc, or the WASI SDK) and an LLVM clang where Ash's own runtime build looks for them. A machine without a sysroot builds a driver that cannot target wasm, and says so. A release places the object beside the binary, at `wasm32-wasip1/caribou_runtime.o`, as Ash places its own.

The workspace's `.cargo/config.toml` builds wasm targets with the flags Ash's config uses. Ash's exceptions are `setjmp`, and the backend rewrites them into the exceptions proposal. caribou-wren is built without its `host` feature there: no JIT tiers and no threads, only compiled Wren on one thread.

## Continuous Integration

`.github/workflows` holds three workflows. `ci.yml` runs the tests on every push: the stable crates, the whole workspace on nightly, rustfmt and clippy with warnings denied, and the core on wasm32. `nightly.yml` publishes a release build of the `caribou` command for macOS, Linux and Windows as the rolling `nightly` pre-release (one `caribou-nightly-<target>` archive per platform, which `install.sh` and `install.ps1` at the repository root fetch and verify), built with `--features llvm` (see below), LLVM installed as the runtimes' own releases install it. `pages.yml` publishes `site/` with the installers beside the page. `bench.yml` runs the benchmarks nightly on macOS and Linux into the run's summary. Each job checks out `ash` and `zyntax` beside the repository at the revs `Cargo.toml` pins.

## JIT Tiers & LLVM Configuration

A default build runs Ash on its interpreter and Cranelift tier and WrenLift on its interpreter and Cranelift tiers, and links no LLVM. `--features llvm` on `caribou-driver` adds both runtimes' LLVM tiers, as their own releases ship them: `caribou-ash/llvm` turns on Ash's, `caribou-wren/llvm` WrenLift's. It needs LLVM 21 on the build machine (`LLVM_SYS_211_PREFIX`), links it statically, and grows the command by LLVM's size. The nightly archives are built this way; on macOS the script bundles the Homebrew dylibs the build still references (z3), and on Windows the DLLs LLVM imports (zlib, zstd, libxml2).
