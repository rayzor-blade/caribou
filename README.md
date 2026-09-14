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
| `caribou-driver` | Runs a program: one world with every resident language, from the project's own layout. The `caribou` command is its front. |

The core builds on stable Rust and depends on `caribou_abi`, `krio` and
`libc`. Neither Cranelift nor LLVM is in its graph.

## Status

Ash and WrenLift both run on the core today, and each has been checked
against its own binary. Ash's parity corpus and the Haxe conformance suite
come out identical through `caribou-ash`. WrenLift's benchmarks come out
identical through `caribou-wren`, including under collector stress, and a
collection costs the same as it does under WrenLift's own collector.

The bridge lets one language call another, and the module registry lets
each import the other's classes the ordinary way. A Wren program writes
`import "game:Player" for Player`. A Haxe program built with `-lib caribou`
writes `import game.hud.Hud` for a Wren module at `src/game/hud.wren`, and
a Wren method says what it exposes with `#export = "add(n: Num) -> Num"`,
or nothing when the runtime can tell. What is not there yet: hot reload,
the plugin loader, and the Zyntax adapter.

## Building

```sh
cargo build -p caribou            # the core, stable
cargo test -p caribou

cargo +nightly build -p caribou-ash --features runner    # ash on the core
cargo build -p caribou-wren --features runner            # wren_lift on the core
cargo +nightly build -p caribou-driver                   # the caribou command
```

For now the adapters find their runtimes by path; they will become git
dependencies once the runtimes are published. Building `caribou-ash` needs
`LLVM_SYS_211_PREFIX` set, because Ash's build script asks for it even
though the runner links no LLVM.

A program runs from its project directory, and the other languages'
modules are found under the project's class paths and loaded on first
use:

```sh
caribou run bin/game.hl
```

`caribou run --report` prints, when the program ends, what the run did:
the tier each function reached, whether each send across the bridge is
direct or takes the plain path, and what crossed boxed. It is how to see
whether something was optimized without waiting for a build.

Each per-runtime runner takes a program and an execution mode, and
`--no-install` runs the runtime on its own implementation instead, which
is handy for comparing the two:

```sh
target/debug/caribou-ash --mode hybrid game.hl
target/debug/caribou-wren --mode tiered script.wren
```

`cargo bench -p caribou-interop --bench interop` times a call across the
bridge in each direction beside the same call inside each language, per
operation. `--bench swarm` runs a game frame three ways, engine and
gameplay in Haxe, in Wren, and split between them, and reports the
frame time of each.

WrenLift's LLVM top tier is off by default, as it is in WrenLift, so a
Wren body runs on its Cranelift baseline. `--features llvm` on
`caribou-wren`, `caribou-driver` or `caribou-interop` turns it on; it
needs LLVM 21 on the build machine, and the whole build then links LLVM
dynamically, Ash's tier included.

The Haxe library lives in `haxe/`. Until it is published, register the
checkout once with `haxelib dev caribou haxe`; a program then builds with
`-lib caribou`, and the library finds the `caribou` command on the path or
in this checkout's target directory.

## Documentation

- [docs/interop.md](docs/interop.md) is the reference for what a program
  writes: namespaces, how each language sees the other's classes, export
  signatures, and what crosses how.
- [docs/architecture.md](docs/architecture.md) is the map of the systems,
  one page each under `docs/architecture/`: the heap, the scheduler, the
  bridge, the adapters, the registry, imports in each direction, and the
  world and driver.
- Issues are tracked inside the repository with
  [git-bug](https://github.com/git-bug/git-bug); `git-bug bug` lists them.

## License

MIT. See [LICENSE](LICENSE).
