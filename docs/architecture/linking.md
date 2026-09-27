# Link-Time Binding

## Overview

At AOT, a cross-language call site does not go through the bridge. `wren_call`, the host entry, the kinds, and the direct sends all exist because the callee is found at run time. At link time, the callee is a symbol, and a send is a plain call with a cast on each side of it. Every side names members by one shared rule, `caribou::link`, so the caller and the callee agree by construction, in the same way the build macro and the publisher already agree on types.

## Symbol Naming

A member's symbol consists of `caribou`, then the language, the module as its language spells it, the class, the kind letter followed by the member's name, and the arity. Each part is preceded by `_`, and each name is written as its length followed by its text:

| Member | Symbol |
|---|---|
| Wren `bench/tally`, `Tally`, `add(_)` | `caribou_4wren_13bench_2ftally_5Tally_m3add_1` |
| Haxe `game.Player`, `Player`, static `spawnAt(x, y)` | `caribou_4haxe_13game_2ePlayer_6Player_t7spawnAt_2` |
| Wren `m`, `C`, setter `hp=(_)` | `caribou_4wren_1m_1C_s2hp_1` |

**Encoding rules:**

* The kind letter is `m` for a method, `g` for a getter, `s` for a setter, `t` for a static, and `c` for a constructor.
* A character that is not valid in a C identifier is written as `_` followed by two hex digits. This includes `_` itself, so no two names produce the same symbol and the separators stay readable.
* The module is the language's own name for it, such as `game.Player` or `game/hud`. This is the name the registry keys the module under once a namespace has resolved, and both the importer and the exporter know it.

## Calling Conventions and Casts

Types are erased at the machine level, so each side keeps its own compiled types. The callee is its language's own compiled function, exported under its link symbol in its own calling convention. The caller casts each value at the boundary. Both sides' types are known when the program is built, so the build chooses every cast, and nothing is decided at run time.

* **A plugin** takes and returns its ABI's C types (`caribou_abi`): `double`, `int32_t`, `bool`, and a core string as a pointer. An instance member takes its receiver first.
* **A Wren member** takes and returns WrenLift's NaN-boxed values, one `uint64_t` each. A `Num` is its `double`'s bits. An instance member takes its receiver first.
* **Haxe** calls through the program's `caribou` natives. Each one is linked to the member's symbol rather than bound at run time.

A cast is a function that takes the value and the program's `hl_type` for its Haxe side. That is how a cast that produces a Haxe object allocates one, since a compiled program has no other way to name its types. Casts go directly between two languages' forms: a Haxe `String` becomes a Wren string in one step, not through the core's. A number passes unchanged wherever the two sides' words agree.

After the call, a check raises what the callee left pending, such as a plugin's `host::raise` or a Wren `Fiber.abort`, into the Haxe caller as an exception.

Members with `Dyn` or `Fun` types have no static form and stay on the bridge. The run report names each one.

## Building a Program

`caribou build --target wasm32-wasip1` builds a program in these steps:

1. **Plugins.** A plugin named by its crate is built together with `caribou-runtime` as one crate graph. Two Rust libraries built apart would each carry std and an allocator. The graph enables `caribou_abi`'s `linked` feature, under which each plugin exports its entry under a name of its own and a constructor of the graph registers it.
2. **Wren modules.** The project's Wren modules are compiled by WrenLift as one library object. Each module's link name is its path under the sources.
3. **Joining.** The runtime staticlib is joined with WASI's libc into one relocatable object, and the Wren object is joined to it.
4. **Linking.** Ash's AOT compiles the Haxe program and links it against that object with `ash_wasm_link`.

At run time, the program's `main` brings the heap up, initialises the Haxe module, and calls the runtime's `program_start`. That call is a slot of Ash's seam, which caribou fills. Caribou starts WrenLift's VM there and runs the linked modules' bodies before the Haxe entry.

Neither target embeds an interpreter. On AOT and wasm, every module is compiled and linked. Loading a module from source or bytecode belongs to hosted runs.

## What Remains at Run Time

The following are still handled at run time regardless of linking:

* **Object identity.** The shadow word and a record's descriptor are properties of the shared heap, not of dispatch. Creating a face becomes a direct call, but its bookkeeping remains.
* **Errors.** A callee's error is left pending and raised into the caller after the call returns. It does not unwind across the other language's frames.
* **Untyped members.** Every member with a `Dyn` or `Fun` type. The report names them.
