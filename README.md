<p align="center">
<img style="display: block;" src="assets/caribou.png" alt="Caribou logo" width="250"/>
</p>

<h1 align="center">Caribou</h1>

<p align="center">A shared runtime core for game and multimedia scripting.</p>

Caribou is the substrate under a family of language runtimes: one heap, one
scheduler, one module registry, one native plugin boundary, shared by
[Ash](https://github.com/rayzor-blade/ash) (Haxe, via HashLink bytecode),
[WrenLift](https://github.com/wrenlift/WrenLift) (Wren) and Zyntax (DSLs,
with Lua and Python built on it). A game or engine written in one of these
languages can load code written in the others, pass objects between them,
call across the boundary, and reload any of it while the program runs.

## How it fits

No runtime depends on Caribou. Each keeps its own collector, scheduler,
build and test suite, and exposes a **seam**: a versioned table of the
entry points its own code reaches its runtime through, filled with its own
implementations by default. A Caribou adapter crate depends on the runtime
and installs the core's heap and scheduler into that table before the
first allocation. Nothing in a runtime's manifest names Caribou, and every
runtime still passes its own suite with nothing installed.

The application language drives. A Haxe program is the default driver of a
world: it owns the entry point, the frame loop and publishing, and the
other languages run as spokes it loads. Spokes reload without a Rust
build, even inside a shipped binary.

## Crates

| Crate | Role |
|---|---|
| `caribou_abi` | `no_std`, zero dependencies. The layouts and constants every runtime, plugin and the core agree on: HashLink's `hl.h` structs with size and offset tests, the NaN-boxed `Value`, allocation kinds, the plugin descriptor table, error kinds. |
| `caribou` | The core. An Immix heap that is non-moving, conservative by default and precise through type descriptors; a scheduler over krio tasks, stackful and stackless alike; the object protocol every heap object answers; the `World` a driver holds. |
| `caribou-ash` | Hosts Ash: fills `ash_std`'s seam with the core's heap and scheduler. Nightly, because `ash_std` needs it. |
| `caribou-wren` | Hosts WrenLift: fills `wren_lift`'s seam with the core heap under its Immix strategy. |

The core builds on stable Rust and depends on `caribou_abi`, `krio` and
`libc`. Neither Cranelift nor LLVM is in its graph.

## Status

Ash and WrenLift both run on the core, verified against their own
binaries: Ash's parity corpus and the Haxe conformance suite are identical
through `caribou-ash`; WrenLift's benchmark corpus is identical through
`caribou-wren`, including under collector stress, at the same per-cycle
cost as its own collector. The cross-language bridge, the module registry
with hot reload, the plugin loader and the Zyntax adapter are the work in
progress.

## Building

```sh
cargo build -p caribou            # the core, stable
cargo test -p caribou

cargo +nightly build -p caribou-ash --features runner    # ash on the core
cargo build -p caribou-wren --features runner            # wren_lift on the core
```

The adapters depend on their runtimes by path until those are published as
git dependencies; `caribou-ash` also needs `LLVM_SYS_211_PREFIX` set for
Ash's build script even though it links no LLVM. The runners take a program
and an execution mode, and `--no-install` runs the runtime on its own
implementation for comparison:

```sh
target/debug/caribou-ash --mode hybrid game.hl
target/debug/caribou-wren --mode tiered script.wren
```

## Documentation

- [docs/architecture.md](docs/architecture.md): the systems as built, and
  the contracts the unbuilt ones are written against.
- Issues are tracked with [git-bug](https://github.com/git-bug/git-bug) in
  the repository itself: `git-bug bug` lists them.

## License

MIT. See [LICENSE](LICENSE).
