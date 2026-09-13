<p align="center">
<img style="display: block;" src="assets/caribou.png" alt="Caribou logo" width="250"/>
</p>

<h1 align="center">Caribou</h1>

<p align="center">A shared runtime core for game and multimedia scripting.</p>

Caribou is the shared core under three language runtimes:
[Ash](https://github.com/rayzor-blade/ash), which runs Haxe through
HashLink bytecode; [WrenLift](https://github.com/wrenlift/WrenLift), which
runs Wren; and Zyntax, which runs DSLs and will carry Lua and Python. It
gives them one heap, one scheduler, one module registry and one way to
load native plugins.

The point is mixing them. A game can be written mostly in Haxe with its
gameplay scripted in Wren, hand objects back and forth between the two,
and reload the scripts while it is running.

## How it works

Caribou never becomes a dependency of the runtimes it serves. Ash and
WrenLift keep their own collectors, schedulers, builds and test suites,
and nothing in their manifests mentions Caribou at all.

Instead, each runtime exposes a seam. The seam is a table of function
pointers covering everything the runtime does to its heap and its
scheduler, and by default every entry points at the runtime's own code.
When Caribou hosts a runtime, a small adapter crate fills that table with
the core's heap and scheduler before anything is allocated. Run the
runtime on its own and the table is never touched, so its existing test
suite keeps passing exactly as before.

In a Caribou program, one language is in charge. Usually that is Haxe: the
Haxe application owns the entry point, the frame loop and the shipped
binary, and it loads the other languages as scripts. Those scripts can be
edited and reloaded while the game runs, without rebuilding anything in
Rust, and that stays true after the game has shipped.

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

Ash and WrenLift both run on the core today, and each has been checked
against its own binary. Ash's parity corpus and the Haxe conformance suite
come out identical through `caribou-ash`. WrenLift's benchmarks come out
identical through `caribou-wren`, including under collector stress, and a
collection costs the same as it does under WrenLift's own collector.

The bridge lets one language call another, and the module registry lets
a Wren program import a Haxe class with an ordinary `import`. What is not
there yet: the other direction, hot reload, the plugin loader, and the
Zyntax adapter.

## Building

```sh
cargo build -p caribou            # the core, stable
cargo test -p caribou

cargo +nightly build -p caribou-ash --features runner    # ash on the core
cargo build -p caribou-wren --features runner            # wren_lift on the core
```

For now the adapters find their runtimes by path; they will become git
dependencies once the runtimes are published. Building `caribou-ash` needs
`LLVM_SYS_211_PREFIX` set, because Ash's build script asks for it even
though the runner links no LLVM.

Each runner takes a program and an execution mode. Pass `--no-install` to
run the runtime on its own implementation instead, which is handy for
comparing the two:

```sh
target/debug/caribou-ash --mode hybrid game.hl
target/debug/caribou-wren --mode tiered script.wren
```

## Documentation

- [docs/architecture.md](docs/architecture.md) describes each system as it
  is built, and for the ones not built yet, the contract they will be
  written against.
- Issues are tracked inside the repository with
  [git-bug](https://github.com/git-bug/git-bug); `git-bug bug` lists them.

## License

MIT. See [LICENSE](LICENSE).
