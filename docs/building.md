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

## Continuous Integration

`.github/workflows` holds three workflows. `ci.yml` runs the tests on every push: the stable crates, the whole workspace on nightly, rustfmt and clippy with warnings denied, and the core on wasm32. `nightly.yml` publishes a release build of the `caribou` command for macOS, Linux and Windows as the rolling `nightly` pre-release. `bench.yml` runs the benchmarks nightly into the run's summary. Each job checks out `ash` and `zyntax` beside the repository at the revs `Cargo.toml` pins.

## JIT Tiers & LLVM Configuration

WrenLift executes on Cranelift by default. To enable the LLVM optimizing tier, build `caribou-wren`, `caribou-driver`, or `caribou-interop` with `--features llvm`. This requires an LLVM 21 installation and links LLVM dynamically across all JIT components (including Ash).
