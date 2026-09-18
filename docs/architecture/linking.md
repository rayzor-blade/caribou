# Link-Time Binding

## Overview

At AOT, a cross-language call site does not go through the bridge. `wren_call`, the host entry, the kinds, and the direct sends all exist because the callee is found at run time. At link time, the callee is a symbol, and a send is a plain call. Each runtime's AOT emits its half of the call against one shared rule, `caribou::link`, so the two sides agree by construction, in the same way the build macro and the publisher already agree on types.

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

## C Signatures

The member's types determine its C signature (`link::CType`):

| Registry Type | C Type |
|---|---|
| `Void` | `void` |
| `Float` | `double` |
| `Int` | `int32_t` |
| `Bool` | `bool` |
| `Str` | `caribou_str *`, a core string |
| `Object(_)` | `void *`, the object as its own language holds it |
| `Array(_)`, `Function {..}` | `uint64_t`, a bridge value |
| `Dyn`, `Fun` | None. The member stays on the bridge |

An instance member takes its receiver first, as a `void *`. A constructor returns a `void *`. A setter returns nothing. A member with a `Dyn` or `Fun` anywhere in its signature has no static form. Its thunk calls the run-time bridge instead, and `Link::dynamic` lists the affected positions, counted from one, with 0 meaning the result. The run report uses this to say, per site, whether the member links statically or stays dynamic, and which type to declare to fix it.

## The Two Halves

* **WrenLift's AOT** emits the definition: a C-ABI thunk per exported member. The thunk takes the C arguments (a Num is passed as its raw bits), calls the compiled body, and returns the result in C form. For a Haxe class that Wren imports, it emits an undefined reference: `Bench.add(s)` compiles to `call caribou_4haxe_...`.
* **Ash's AOT** emits the mirror image. A `caribou` native becomes an extern C function with the declared signature under that name, and an exported Haxe static or method gets a C-ABI symbol by the same rule. The published set seeds reachability analysis, so no body that a Wren module might call is removed as dead code.

A standard linker resolves the references between the two object files. The build step, which runs both AOTs and then lld, belongs to the driver. A custom linker is only needed for wasm, where `ash_wasm_link` takes WrenLift's module as well. When both sides emit bitcode, lld with LTO inlines the thunk and the callee into the caller: `Bench.add(s)` in Wren compiles down to `s + 1`.

## What Remains at Run Time

The following are still handled at run time regardless of linking:

* **Object identity.** The shadow word and a record's descriptor are properties of the shared heap, not of dispatch. Creating a face becomes a direct call, but its bookkeeping remains.
* **Traps.** The trap on a Wren-to-Haxe call stays until Ash's exceptions can cross a native frame without a long jump. Until then, the thunk arms the trap, as the bridge's guard does today.
* **Untyped members.** Every member with a `Dyn` or `Fun` type. The report names them.
