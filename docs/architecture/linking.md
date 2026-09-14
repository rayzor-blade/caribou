# Linking: a member at link time

At AOT a cross-language site has no bridge. `wren_call`, the host
entry, the kinds and the direct sends exist because the callee is found
at run time; at link time it is a symbol, and a send is a call. Each
runtime's AOT emits its half against one rule, `caribou::link`, so the
two sides agree by construction, as the build macro and the publisher
already agree on types.

## The symbol

A member's symbol is `caribou`, then the language, the module as its
language spells it, the class, the kind's letter with the member's
name, and the arity, each after a `_`, and each name as its length and
its text:

| Member | Symbol |
|---|---|
| Wren `bench/tally`, `Tally`, `add(_)` | `caribou_4wren_13bench_2ftally_5Tally_m3add_1` |
| Haxe `game.Player`, `Player`, static `spawnAt(x, y)` | `caribou_4haxe_13game_2ePlayer_6Player_t7spawnAt_2` |
| Wren `m`, `C`, setter `hp=(_)` | `caribou_4wren_1m_1C_s2hp_1` |

The letter is `m` for a method, `g` a getter, `s` a setter, `t` a
static and `c` a constructor. A character that is not a C identifier's
is written as `_` and two hex digits, `_` itself included, so no two
names share a symbol and the separators stay readable. The module is
the language's own name for it, `game.Player` or `game/hud`: what the
registry keys the module under once a namespace has resolved, which
both an importer and an exporter know.

## The signature

The member's types give its C signature (`link::CType`):

| Registry type | C |
|---|---|
| `Void` | `void` |
| `Float` | `double` |
| `Int` | `int32_t` |
| `Bool` | `bool` |
| `Str` | `caribou_str *`, a core string |
| `Object(_)` | `void *`, the object as its own language holds it |
| `Array(_)`, `Function {..}` | `uint64_t`, a bridge value |
| `Dyn`, `Fun` | none: the member stays on the bridge |

An instance member takes its receiver first, as `void *`. A constructor
returns `void *`; a setter returns nothing. A member with a `Dyn` or
`Fun` anywhere has no static form: its thunk calls today's bridge, and
`Link::dynamic` names the positions, counted from one, with 0 for the
result, so the run report can say per site whether it links or stays
dynamic, and which type to declare.

## The halves

wren_lift's AOT emits the definition: a C-ABI thunk per exported member
that takes the C arguments (a Num is its own bits), calls the compiled
body, and answers in the C form. For a Haxe class it imports, it emits
the reference, undefined: `Bench.add(s)` is `call
caribou_4haxe_...`. Ash's AOT emits the mirror: a `caribou` native is an
extern C function of the declared signature under that name, and an
exported Haxe static or method gets a C-ABI symbol by the same rule;
the published set seeds reachability, so no body a Wren module may call
is dropped as dead.

A standard linker resolves the forward references between the two
object files; the build step, both AOTs then lld, is the driver's. A
custom linker is the tool only for wasm, where `ash_wasm_link` takes
wren_lift's module too. When both emit bitcode, lld with LTO inlines
thunk and callee into the caller: `Bench.add(s)` in Wren compiles to
`s + 1`.

## What stays at run time

Object identity: the shadow word and a record's descriptor are facts
of the shared heap, not of dispatch; a face's creation gets direct, its
bookkeeping stays. The trap on a Wren→Haxe call, until Ash's
exceptions cross a native frame without a long jump; the thunk arms it
meanwhile, as the bridge's guard does today. And every untyped member,
which the report names.
