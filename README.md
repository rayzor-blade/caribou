<p align="center">
<img style="display: block;" src="assets/caribou.png" alt="Caribou logo" width="250"/>
</p>

<h1 align="center">Caribou</h1>

<p align="center">A shared runtime core for game and multimedia scripting.</p>

Caribou is the runtime core under a family of language runtimes. It
provides one heap, one scheduler, one module registry and one native plugin
boundary. Three runtimes share it: [Ash](https://github.com/rayzor-blade/ash)
runs Haxe through HashLink bytecode, [WrenLift](https://github.com/wrenlift/WrenLift)
runs Wren, and Zyntax runs DSLs, with Lua and Python built on it.

A game written in one of these languages can load code written in the
others. It can pass objects between them and call across the boundary. It
can reload any of that code while the program runs.

## How it fits

No runtime depends on Caribou. Each runtime keeps its own collector, its
own scheduler, its own build and its own test suite. Each one exposes a
**seam**: a table of function pointers for the operations its code performs
on its runtime. By default the table holds the runtime's own functions.

Caribou fills that table. An adapter crate depends on the runtime, and
installs the core's heap and scheduler into the table before the first
allocation. The runtime's manifest never names Caribou. With nothing
installed, the runtime still passes its own suite.

The application language drives. By default that is a Haxe program. It owns
the entry point, the frame loop and publishing. The other languages are
spokes it loads. A spoke reloads without a Rust build, even inside a
shipped binary.

## Crates

| Crate | Role |
|---|---|
| `caribou_abi` | `no_std`, zero dependencies. The layouts and constants every runtime, plugin and the core agree on: HashLink's `hl.h` structs with size and offset tests, the NaN-boxed `Value`, allocation kinds, the plugin descriptor table, error kinds. |
| `caribou` | The core. It holds the heap, the scheduler, the object protocol and the `World` a driver uses. The heap is Immix: non-moving, conservative by default, precise where a type descriptor asks for it. The scheduler runs stackful fibers and stackless state machines on one queue. |
| `caribou-ash` | Hosts Ash: fills `ash_std`'s seam with the core's heap and scheduler. Nightly, because `ash_std` needs it. |
| `caribou-wren` | Hosts WrenLift: fills `wren_lift`'s seam with the core heap under its Immix strategy. |

The core builds on stable Rust and depends on `caribou_abi`, `krio` and
`libc`. Neither Cranelift nor LLVM is in its graph.

## Status

Ash and WrenLift both run on the core. Each is verified against its own
binary. Ash's parity corpus and the Haxe conformance suite give identical
results through `caribou-ash`. WrenLift's benchmark corpus gives identical
results through `caribou-wren`, including under collector stress, and a
collection cycle costs the same as under its own collector.

Still in progress: the cross-language bridge, the module registry with hot
reload, the plugin loader and the Zyntax adapter.

## Building

```sh
cargo build -p caribou            # the core, stable
cargo test -p caribou

cargo +nightly build -p caribou-ash --features runner    # ash on the core
cargo build -p caribou-wren --features runner            # wren_lift on the core
```

The adapters depend on their runtimes by path for now. They move to git
dependencies once the runtimes are published. `caribou-ash` needs
`LLVM_SYS_211_PREFIX` set, because Ash's build script asks for it, even
though the runner links no LLVM.

Each runner takes a program and an execution mode. The `--no-install` flag
runs the runtime on its own implementation instead, for comparison:

```sh
target/debug/caribou-ash --mode hybrid game.hl
target/debug/caribou-wren --mode tiered script.wren
```

## Documentation

- [docs/architecture.md](docs/architecture.md) describes each system as
  built. For systems not yet built, it states the contract they are written
  against.
- Issues live in the repository, tracked with
  [git-bug](https://github.com/git-bug/git-bug). Run `git-bug bug` to list
  them.

## License

MIT. See [LICENSE](LICENSE).
