# Interop conventions

How the languages of a Caribou program see each other. This is the
reference for what a program writes and what it can expect; the
mechanism behind each rule is in [architecture.md](architecture.md).

## Running a program

From the project directory:

```sh
caribou run bin/game.hl
```

The project's layout is the configuration. The source roots are the
class paths of the `.hxml` files in the directory and beside the
program, or `src` when there is no `.hxml`. Every directory under a root
is a namespace, and so is every namespace the program imports. A module
of another language loads the first time the program uses it. An
embedder does the same through `caribou_driver::Session`.

## Namespaces and modules

A program reaches another language's module through a namespace. The
driver derives the namespaces from the project; an embedder building its
own world configures them:

```rust
World::new(Config {
    namespaces: vec![Namespace {
        name: "game".into(),
        langs: vec!["haxe".into(), "wren".into()],
        modules: None,
    }],
    ..Config::default()
})
```

- A namespace covers one or more languages, in the order given. A module
  name is looked up in each language in turn.
- With `modules` set, only the listed modules resolve through the
  namespace. With `None`, every module of its languages does.
- Every registered language is also a namespace under its own name, with
  no configuration: `haxe:game.Player` and `wren:hud` always resolve.
- A module is addressable by its own name and, when its name begins with
  the namespace's name and a dot, by the remainder. So `game:Player` finds
  the Haxe module `game.Player`, and `game:hud` finds the Wren module
  `hud`.

A Haxe module is one class, named after it: `game.Player`. A Wren module
is a file, named as the VM loaded it: `hud`, or `ui/hud` for a file in a
directory, with `/` as Wren spells an import.

## Wren using Haxe

```wren
import "game:Player" for Player

var p = Player.new("ada")
p.hit(30)
p.hp = 70
System.print(p.name)
var q = Player.spawnAt(2, 2)

class Hero is Player {
  construct new(name) {
    super(name)
  }
}
```

- Every class of the Haxe program is published, except the runtime's own
  under `hl.` and `haxe.`, class companions, and `String`, which crosses
  as a value.
- The constructor is `new` with the constructor's arity.
- A static method is a static of the same name and arity.
- A field is a getter and a setter of its name: `p.hp` and `p.hp = 70`.
- An instance method is a method of its name and arity. Haxe has no
  overloads, so there is exactly one of each name.
- A Wren class may extend an imported class. Its constructor calls
  `super(...)` with the Haxe constructor's arguments.
- A Haxe throw inside a call aborts the fiber with the exception's
  message. So does an argument Haxe cannot take, such as a string where an
  `Int` is declared. `Fiber.try` sees the message.
- A Haxe object is an instance of the imported class. Two crossings of one
  Haxe object are two Wren instances today; identity is not kept in this
  direction yet.

## Haxe using Wren

A program adds `-lib caribou` and puts its Wren modules on the classpath.
Nothing else is declared.

```
src/
  Main.hx
  game/
    hud.wren
```

```haxe
import game.hud.Hud;

var h = new Hud(3);
h.add(4);
h.score = 10;
var b = Hud.best(h, Hud.make(1));
```

### Where a module lands

The file's path is its package, as for a Haxe module.

| File | Haxe package | Namespace | Wren module |
|---|---|---|---|
| `src/game/hud.wren` | `game.hud` | `game` | `hud` |
| `src/game/ui/hud.wren` | `game.ui.hud` | `game` | `ui/hud` |
| `src/hud.wren` | `wren.hud` | `wren` | `hud` |

The first directory under the classpath is the namespace the runtime
resolves the module through; the rest is the module's name. The runtime
must load the module under that name, and the world's namespace must
list `wren`. A file at the classpath root is under Wren's own namespace.

### What a class gets

Every Wren class of the module becomes a Haxe class of the same name.

| Wren member | Haxe member |
|---|---|
| `construct new(a, b)` | `new Hud(a, b)` |
| any other `construct name(...)` | `static function name(...):Hud` |
| `name(a, b)` | `function name(a, b)` |
| `name` | property `name` with a getter |
| `name=(v)` | property `name` with a setter |
| `static name(...)` | `static function name(...)` |
| `static name` | static property `name` |
| operators, `[...]`, `[...]=(...)` | not exported |

- Parameters are `Dynamic` and results are `Dynamic` unless the member
  says otherwise (next section) or the runtime can tell.
- Two members of one name and different arities cannot both be one Haxe
  method. The second keeps its name with the arity appended.
- A subclass gets its superclass's members from the same module, but is
  not a Haxe subclass of it: every emitted class extends `caribou.Ref`.
- A member takes at most six parameters, a static at most seven.

### Export signatures

A Wren member says what it exposes in one attribute:

```wren
#export = "add(n: Num) -> Num"
add(n) { _score = _score + n }
```

The value is a signature:

| Form | Member |
|---|---|
| `name(a: T, b) -> R` | method, static or constructor |
| `name -> R` | getter |
| `name=(v: T)` | setter |

- `name` is the name Haxe sees. It may differ from Wren's: the member is
  exported under it, and the runtime still calls the Wren method.
- One entry per Wren parameter, in order. `a` names it, `a: T` also types
  it, `_: T` types it and keeps the source's name. Types match by
  position, so the runtime reads the same attribute off the running
  class.
- `-> R` types the result. Without it, the result is what inference gives.
- The attribute must fit its member: the same shape, and one entry per
  parameter. Otherwise the description is refused with the reason.
- The attribute is optional. Without it a member is exported under its
  own name, with the source's parameter names.

Type names are Wren's own, or a class of the same module:

| In `#export` | Registry | Haxe |
|---|---|---|
| `Num` | `Float` | `Float` |
| `Bool` | `Bool` | `Bool` |
| `String` | `Str` | `String` |
| `List` | `Array(Dyn)` | `Array<Dynamic>` |
| `Fn` | `Fun` | `Dynamic` |
| a class of the module | `Object("module.Class")` | that class |
| anything else, or nothing | `Dyn` | `Dynamic` |

### Inference

A result needs no `#export` when the runtime can tell it from the body.
It runs wren_lift's own inference and takes:

- a literal: `count { 0 }` is `Num`, `flag { true }` is `Bool`;
- an interpolation: `"%(prefix): %(_score)"` is `String`;
- a constructor call: `Hud.new(0)` is `Hud`;
- a field whose assignments are typed, or another method's result.

Parameters are only ever declared: they are where inference starts. A
field assigned from an untyped parameter is `Dyn`, and so is every result
built on it.

### Values

| Wren | crossing | Haxe |
|---|---|---|
| `Num` | by value | `Float` (an `Int` on the way in becomes a `Num`) |
| `Bool` | by value | `Bool` |
| `String` | by value, copied | `String` |
| `null` | | `null` |
| an object | by reference | the class emitted for it, else `caribou.Ref` |
| a Haxe object coming back | by reference | the same Haxe object |

- A Wren object reaching Haxe twice is the same Haxe object, and a Haxe
  object of an emitted class going into Wren is the Wren object it
  stands for. Haxe `==` works.
- The Haxe object keeps the Wren object alive. Wren's collector sees
  what Haxe holds.
- A Wren abort inside a call is thrown into Haxe as a `String` with the
  message. A Haxe exception that crossed into Wren and comes back is
  rethrown as itself.

## Names the runtime binds

A program never writes these, but they are the contract between the
emitted Haxe class and the runtime, and they appear in a `.hl`'s native
table and in errors.

Each member of an emitted class is a native of the `caribou` library
named `namespace:module.Class.signature`, where the signature is the
member's Wren signature:

| Member | Native name |
|---|---|
| `add(n)` | `game:hud.Hud.add(_)` |
| `score` | `game:hud.Hud.score` |
| `score=(v)` | `game:hud.Hud.score=(_)` |
| `static best(a, b)` | `game:hud.Hud.static:best(_,_)` |
| `construct new(score)` | `game:hud.Hud.construct:new(_)` |

The name is the whole binding. Nothing has to be published before the
program starts, and a module published again answers the next call.

## Describing a module

`caribou describe src/game/hud.wren` prints the module's
interface as JSON: the classes, their members with kind, Wren signature,
parameter names and types, and result type. It is what the Haxe library
reads, and it is the same shape the runtime publishes to the registry
when the module loads, so the two cannot disagree.
