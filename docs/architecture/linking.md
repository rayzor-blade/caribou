# Link-Time Binding

## Overview

At AOT, a cross-language call site does not go through the bridge. The host entry, member kinds, and dynamic sends exist because a hosted build finds the callee at run time. At link time, the callee is a symbol, and a send is a plain call with a cast on each side of it. Every frontend names members by one shared rule, `caribou::link`, so the caller and callee agree by construction, in the same way the build macro and publisher already agree on types.

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
* **A compiled language member** uses that frontend's native calling convention. Its Caribou adapter supplies the casts between those values and the caller's values. WrenLift, for example, uses one NaN-boxed `uint64_t` per value.
* **Haxe** calls through the program's `caribou` natives. Each one is linked to the member's symbol rather than bound at run time.

A cast is a function that takes the value and the program's `hl_type` for its Haxe side. That is how a cast that produces a Haxe object allocates one, since a compiled program has no other way to name its types. Casts go directly between two languages' forms: a Haxe `String` becomes a Wren string in one step, not through the core's. A number passes unchanged wherever the two sides' words agree.

After the call, a check raises what the callee left pending, such as a plugin's `host::raise` or a Wren `Fiber.abort`, into the Haxe caller as an exception.

Members with `Dyn` or `Fun` types have no static form and stay on the bridge. The run report names each one.

## Calls into Haxe

A compiled language calls Haxe the same way, with the sides swapped. Only Ash knows the program's `hl_type`s, so the casts run on Haxe's side:

* **Ash exports each Haxe member another language calls.** The export is a thunk under the member's link symbol, in the caller's convention: for WrenLift, one NaN-boxed word per argument, the receiver first, and one word for the result (`ash_core::host_export`). Inside, the thunk casts each word to the Haxe type, calls the member, and casts the result back. A Haxe throw is caught there and becomes the caller's error.
* **The caller treats the Haxe class as a module of its own that calls those symbols.** An `import "bench:Bench" for Bench` in compiled Wren becomes the module `haxe:Bench`. WrenLift makes that class at start-up, and each of its members calls its symbol (`AotForeignModule`).
* **The driver decides what crosses.** It describes the program's Haxe classes from the bytecode, finds each Wren import that names one, picks the casts by type, and hands Ash the exports and WrenLift the classes (`caribou-driver`'s `foreign.rs`). A Haxe object that crosses into Wren becomes an instance of that class, held through its cell as in a hosted run.

A Wren class cannot extend such a class.

The `caribou` library's own natives, which `caribou.Future` and `caribou.Sequence` declare, link to entry points in `caribou-ash` that run the same bridge operations a hosted run's natives run. A plugin's future crosses as a `caribou.Future`. A foreign object that crosses as a `Dynamic`, such as an awaited result, needs the Haxe class that stands for its type. A hosted run finds that class by reading the program. A compiled program can't, so each face class names its `hl_type` to the runtime as the program starts.

## Building a Program

`caribou build --target wasm32-wasip1` builds a program in these steps:

1. **Language objects.** Each resident frontend compiles its modules into relocatable objects and describes the exports in those objects. Its artifact carries the adapter that maps those exports' machine types to Haxe calls. WrenLift currently implements this contract. Zyntax frontends still need an object-emission mode; their WASM backend currently produces final runtime modules.
2. **Haxe object.** Ash lowers the `.hl` program into its own relocatable object and binds its Caribou natives to the described member symbols.
3. **One final link.** Caribou passes all language objects to Ash's AOT request. `ash_wasm_link` links them together with the Caribou runtime and WASI support. Caribou does not prejoin one language into the runtime.
4. **Native plugins.** A plugin crate joins the program in one of two ways, chosen per plugin in the project file:
   * **Linked** (the default). The plugin is built into the program's runtime, in one crate graph with `caribou-runtime`, because two Rust libraries built apart would each carry std and an allocator. Under `caribou_abi`'s `linked` feature, each plugin exports its entry under a name of its own, and a constructor of the graph registers it. Calls into it are direct.
   * **Side module** (`link = "side"`). The plugin is built as a position-independent `dylink.0` side module beside the program, under `caribou_abi`'s `side_module` feature. It allocates with the program's `malloc`, and it takes the host table from the program by importing `caribou_host_table`. Each call goes through a slot that the program fills at startup with the member's symbol, found in that module (`HostLink.library`). A side module loads only into a single-threaded `wasm32-wasip1` program under a host that loads libraries, such as `caribou run`.

A plugin can also ship its part in a page: JavaScript that runs beside the program in a browser. For example, the GPU plugin's part holds the browser's WebGPU. The plugin's build writes these files to `$OUT_DIR/page/`, with `<name>.mjs` as the entry. The driver finds them through cargo's build-script messages and writes them beside the program, in either mode. [A Plugin in a Page](plugins.md#a-plugin-in-a-page) gives the convention.

At run time, the program's `main` brings the heap up, initialises the Haxe module, and calls the runtime's `program_start`. That call is a slot of Ash's seam, which Caribou fills. Each linked language adapter initialises its compiled module state before the Haxe entry. Side modules are loaded by the host before the program starts, and their members are found when it does.

A language included in an AOT program must supply a relocatable-object emitter. No interpreter is embedded as a fallback. Loading a language module from source or bytecode belongs to hosted runs.

## What Remains at Run Time

The following are still handled at run time regardless of linking:

* **Object identity.** The shadow word and a record's descriptor are properties of the shared heap, not of dispatch. Creating a face becomes a direct call, but its bookkeeping remains.
* **Errors.** A callee's error is left pending and raised into the caller after the call returns. It does not unwind across the other language's frames.
* **Untyped members.** Every member with a `Dyn` or `Fun` type. The report names them.
