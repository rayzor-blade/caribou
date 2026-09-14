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
- A static field is a static getter and setter of its name:
  `Player.spawned` and `Player.spawned = 0` read and write the field where
  Haxe keeps it, on the class object.
- A Haxe object is an instance of the imported class, and the same
  instance each time it crosses while Wren holds it, so Wren `==` works
  on it; an instance going back to Haxe is the object it stands for.
- A Haxe array is a Wren `Sequence`: `xs.count`, `xs[i]`, `xs[i] = v`,
  `for (x in xs)`, `xs.toList` and the rest of `Sequence` work on it,
  reading and writing the array where Haxe keeps it. Going back to Haxe
  it is the same array.

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
| `static name` | static property `name` with a getter |
| `static name=(v)` | static property `name` with a setter |
| operators, `[...]`, `[...]=(...)` | not exported |

- Parameters are `Dynamic` and results are `Dynamic` unless the member
  says otherwise (next section) or the runtime can tell.
- Two members of one name and different arities cannot both be one Haxe
  method. The second keeps its name with the arity appended.
- A subclass gets its superclass's members from the same module, but is
  not a Haxe subclass of it: every emitted class extends `caribou.Ref`.
- A member takes at most sixteen parameters, as in Wren.

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
| `Fn(Num, Hud) -> Bool` | `Function` | `(Float, Hud) -> Bool` |
| `Null`, as a result | `Void` | `Void` |
| a class of the module | `Object("module.Class")` | that class |
| a class a namespaced import brings in, `import "swarm:Entity" for Entity` | `Object` of that class | that class: the Haxe class, or the class emitted for the Wren module |
| anything else, or nothing | `Dyn` | `Dynamic` |

A function type spells its parameters' types and its result's, each any
type of this table; `Fn()` takes nothing, and a missing `-> Type` gives
`Dynamic`.

A result declared `Null` is dropped at the crossing. A method called for
its effect wants one: a Wren body answers with its last expression's
value, and a `Dynamic` result boxes a number on every call.

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

## Functions and callbacks

A function crosses by reference, keeps its captured environment, and
comes home as itself.

```wren
// Wren gives Haxe functions: a typed parameter, an untyped one, and a
// static Haxe keeps and fires later from its own code.
Player.twice(Fn.new {|x| x * 3 }, 2)
Player.apply(Fn.new {|s| s + "!" }, "hi")
Player.onHit = Fn.new {|d| System.print("hit for %(d)") }
```

```haxe
// Haxe gives Wren functions, which Wren calls as any Fn.
Hud.onTick(function(n:Dynamic):Dynamic return n * 2);
Hud.twice(function(x:Float):Float return x + 1, 1);
```

- A Wren `Fn` arrives in Haxe as a real function value: `cb(3)` calls
  it, `Reflect.isFunction` says so, and a parameter or field declared
  `Int -> Void` takes it. Read back by Wren, it is the same `Fn`. Where
  the member declares the function's type, `adder() -> Fn(Num) -> Num`,
  the value is a function of that type, `Float -> Float`, and Haxe calls
  it as one of its own, nothing boxed on the way. Declared `Fn`, it is
  the variadic function `Reflect.makeVarArgs` makes, and a call boxes its
  arguments.
- A Haxe function arrives in Wren as an object answering `call(...)` with
  up to eight arguments and `arity`, as a `Fn` does. It is not a `Fn`:
  `cb is Fn` is false.
- A throw inside a callback propagates as any call does: a Haxe throw
  aborts the Wren fiber with its message, a Wren abort is thrown into
  Haxe as a `String`. A result Haxe has no form for is `null`.
- The callee keeps the function alive for as long as it holds it, the
  same as for objects.
- A Wren function belongs to its VM. Called on another VM, or once its VM
  is gone, it raises "belongs to another Wren VM" instead of running. A
  Haxe static that holds a Wren callback must be cleared before that VM
  goes; in a project there is one VM for the program's life.
- `#export = "onTick(cb: Fn)"` maps to `Dynamic` in Haxe; `cb: Fn(Num)`
  to `Float -> Void`.

## Static state

State a class keeps for itself is shared by reference, never copied:
every access goes through the owner, so a read sees the latest write
from either language and a write lands in the owner's storage.

- A Haxe `static var` is a static field of the class. Wren reads and
  writes it as `Player.spawned`.
- A Wren class keeps static state behind a static getter and setter,
  `static count { __count }` and `static count=(v) { __count = v }`,
  which Haxe sees as one static property, `Hud.count`.
- A Wren module-level `var` belongs to no class. Another Wren module
  imports it by name; Haxe does not see it.

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
