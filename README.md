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

Caribou is one heap, one scheduler, one module registry and one plugin loader under three runtimes: [Ash](https://github.com/rayzor-blade/ash) (Haxe on HashLink bytecode), [WrenLift](https://github.com/wrenlift/WrenLift) (Wren) and Zyntax (ZynML and Python today, Lua to come). A Haxe program drives. Its Wren and Zyntax modules import its classes with ordinary imports, it imports theirs the same way, objects cross without copying, and a module reloads while the program runs.

## How it works

The runtimes do not depend on Caribou. Each keeps its own collector, scheduler, tests and build, and exposes a seam: a table of function pointers for allocation, collection and scheduling that points at its own implementation by default. An adapter crate fills the table with the core's before the runtime allocates anything. The runtime's own tests run unchanged; the same binaries run over the core when an adapter is present.

## Crates

| Crate | Toolchain | What it is |
| --- | --- | --- |
| `caribou_abi` | stable, `no_std` | The ABI: HashLink's `hl.h` layouts, the `Value`, allocation tags, errors and the `plugin!` macro. |
| `caribou` | stable | The core: the Immix heap, the fiber scheduler, the object protocol, the registry and the `World`. |
| `caribou-ash` | nightly | Fills `ash_std`'s seam with the core's heap and scheduler. |
| `caribou-wren` | stable | Fills `wren_lift`'s seam with the core's heap and scheduler. |
| `caribou-zyntax` | stable | Zyntax frontends as languages: a snapshot, a `.zyn` grammar or a parser of its own. |
| `caribou-python` | stable | The Python frontend, registered when a root holds a `.py` file. |
| `caribou-plugin` | stable | Native libraries on the ABI as languages of their own. |
| `caribou-driver` | nightly | The `caribou` command: run, build, describe. |

## Using it

```sh
caribou run bin/game.hl              # the program and the modules beside it
caribou run --report bin/game.hl     # what the tiers compiled and how each crossing went
caribou build bin/game.hl            # bin/game.cb: the program, its modules and plugins
caribou run bin/game.cb
```

On the Haxe side, `haxelib dev caribou haxe` once, then `-lib caribou`: `import game.hud.Hud` reaches `src/game/hud.wren`, and in Wren `import "game:Player" for Player` reaches the Haxe class. A plugin built with `caribou_abi::plugin!` and placed in `plugins/` is a language every side can import. The conventions are in [docs/interop.md](docs/interop.md).

## Building

The core builds on stable. `caribou-ash` and the driver need nightly, because `ash_std` does.

```sh
cargo test -p caribou                          # the core alone
cargo +nightly build -p caribou-driver         # the command
cargo +nightly test --workspace                # everything
```

The manifest pins each runtime at one rev and patches `ash` and `zyntax` to sibling checkouts (`../ash`, `../zyntax`) at those revs. Build `ash_std` in the ash checkout first (`cargo build -p ash_std`): `ash_core`'s build script embeds it. That build also runs bindgen, which needs LLVM 21's libclang; set `LLVM_SYS_211_PREFIX` when it is not found on its own.

The core's unit tests also run on `wasm32-wasip1`, under Ash's wasm host:

```sh
cargo test -p caribou --target wasm32-wasip1 --no-run   # prints the .wasm
ash-wasm-run <the .wasm> --test-threads=1
```

`cargo bench -p caribou-interop --bench interop|transfer|swarm` measures the crossings, object transfer and a mixed game frame. `target/debug/caribou-ash --no-install` and `caribou-wren --no-install` run a program on the runtime's own heap, for comparison. `--features llvm` on the Wren side turns on WrenLift's LLVM tier; it needs LLVM 21 and links it dynamically, for Ash too.

The three workflows under `.github/workflows` are the same builds on GitHub Actions: `ci.yml` tests the stable crates, the workspace on nightly and the core on wasm32 on every push; `nightly.yml` publishes the command for macOS, Linux and Windows as the rolling `nightly` pre-release; `bench.yml` runs the benches into the run's summary. Each job checks out `ash` and `zyntax` beside the repository at the pinned revs.

## Documentation

[docs/architecture.md](docs/architecture.md) covers the heap, the scheduler, the bridge, the adapters, the registry, the bundle and the driver. [docs/interop.md](docs/interop.md) is the reference for imports, exports and what crosses. Issues live in the repository under `git-bug`; `git-bug bug` lists them.

## License

MIT. See [LICENSE](LICENSE).
