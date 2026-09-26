# Interoperability Reference

## Overview

This document describes how the languages in a Caribou program see each other. It is the reference for what a program can write and what behavior it can expect. The mechanisms behind each rule are described in [architecture.md](architecture.md).

## Running a Program

Run a project from its directory:

```sh
caribou run
```

The project file, `<name>.cbproj`, declares the project (see the [README](../README.md#running-programs)):

* The entry is a module of one of the languages. Caribou compiles a Haxe entry itself, with `-lib caribou`, and every Haxe module under the roots with it.
* Each language lists its source roots. Every directory under a root is a namespace, and so is every namespace the program imports.
* Wren's hatch packages and the native plugins are declared there too.
* A module from another language loads the first time the program uses it.

A `.hl` named directly (`caribou run bin/game.hl`) runs with its layout found instead: the roots are the class paths of the `.hxml` files in the current directory and next to the program, or `src`.

An embedder gets the same behavior through `caribou_driver::Session`.

## Namespaces & Modules

A program reaches another language's module through a namespace. The driver derives the namespaces from the project layout. An embedder that builds its own world configures them explicitly:

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

**Resolution rules:**

* A namespace covers one or more languages, in the order given. A module name is looked up in each language in turn.
* When `modules` is set, only the listed modules resolve through the namespace. When it is `None`, every module of the namespace's languages resolves.
* Every registered language is also a namespace under its own name, with no configuration. `haxe:game.Player` and `wren:hud` always resolve.
* A module is addressable by its own name. If its name starts with the namespace's name followed by a dot, it is also addressable by the remainder. `game:Player` finds the Haxe module `game.Player`, and `game:hud` finds the Wren module `hud`.

A Haxe module is one class, named after it: `game.Player`. A Wren module is a file, named as the VM loaded it: `hud`, or `ui/hud` for a file in a subdirectory, using `/` as Wren does in imports.

## Wren Using Haxe

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

**What is published:**

* Every class of the Haxe program is published, except the runtime's own classes under `hl.` and `haxe.`, class companions, and `String`, which crosses as a value.
* The constructor becomes `new` with the constructor's arity.
* A static method becomes a static method with the same name and arity.
* A field becomes a getter and a setter of the same name: `p.hp` and `p.hp = 70`.
* An instance method becomes a method with the same name and arity. Haxe has no overloading, so there is exactly one member per name.
* A static field becomes a static getter and setter: `Player.spawned` and `Player.spawned = 0` read and write the field where Haxe stores it, on the class object.

**Behavior:**

* A Wren class can extend an imported class. Its constructor calls `super(...)` with the Haxe constructor's arguments.
* A Haxe exception thrown inside a call aborts the fiber with the exception's message. An argument that Haxe cannot accept, such as a string where an `Int` is declared, does the same. `Fiber.try` receives the message.
* A Haxe object appears as an instance of the imported class. It is the same instance every time it crosses while Wren holds it, so Wren's `==` works on it. An instance that goes back to Haxe is the original object.
* A Haxe array appears as a Wren `Sequence`. `xs.count`, `xs[i]`, `xs[i] = v`, `for (x in xs)`, `xs.toList`, and the rest of `Sequence` work on it and operate on the array where Haxe stores it. Going back to Haxe, it is the same array.

### Buffers

A `caribou.Buffer` shares its bytes across languages: Haxe's is
`haxe.io.Bytes`, and in Wren a buffer from Haxe or a plugin is a `Sequence`
of its bytes, `b[i]`, `b[i] = v`, `b.count`. Wren makes one from its own
typed arrays: a `ByteArray`, `Int32Array`, `Float32Array` or `Float64Array`
passed where a buffer is taken crosses as a buffer over the array's storage,
without a copy. What the callee writes is in the array, and the buffer comes
back to Wren as the same array.

A string passed where a buffer is taken is its bytes: a read-only buffer over
the string's own bytes, without a copy. A function that writes a buffer
refuses a read-only one.

Lua's bytes are its strings, which Lua never changes. A Lua string that is
not UTF-8 text crosses as a read-only buffer over the string itself; one
that is text crosses as a string. A buffer comes into Lua as itself, and
Lua's string library reads it where it lies: `#b`, `string.len`,
`string.byte`, `string.sub` and `string.unpack` take a buffer as they take
a string. Lua has no byte storage it can write, so it makes a buffer from
the core, `require("core.Buffer").Buffer(16)`, for a function to write
into, as Haxe allocates `Bytes` and Wren a typed array.

## Haxe Using Wren

A program adds `-lib caribou` and puts its Wren modules on the classpath. Nothing else needs to be declared.

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

### Module Placement

The file's path determines its package, as it does for a Haxe module.

| File | Haxe Package | Namespace | Wren Module |
|---|---|---|---|
| `src/game/hud.wren` | `game.hud` | `game` | `hud` |
| `src/game/ui/hud.wren` | `game.ui.hud` | `game` | `ui/hud` |
| `src/hud.wren` | `wren.hud` | `wren` | `hud` |

The first directory under the classpath is the namespace the runtime resolves the module through. The rest of the path is the module's name. The runtime must load the module under that name, and the world's namespace must include `wren`. A file at the classpath root is placed under Wren's own namespace.

### Emitted Members

Every Wren class in the module becomes a Haxe class with the same name.

| Wren Member | Haxe Member |
|---|---|
| `construct new(a, b)` | `new Hud(a, b)` |
| Any other `construct name(...)` | `static function name(...):Hud` |
| `name(a, b)` | `function name(a, b)` |
| `name` | Property `name` with a getter |
| `name=(v)` | Property `name` with a setter |
| `static name(...)` | `static function name(...)` |
| `static name` | Static property `name` with a getter |
| `static name=(v)` | Static property `name` with a setter |
| Operators, `[...]`, `[...]=(...)` | Not exported |

* Parameters and results are `Dynamic` unless the member declares a type (see the next section) or the runtime can infer one.
* Two members with the same name and different arities cannot both become one Haxe method. The second one keeps its name with the arity appended.
* A subclass receives its superclass's members from the same module, but it is not a Haxe subclass of it. Every emitted class extends `caribou.Ref`.
* A member takes at most sixteen parameters, as in Wren.

### Export Signatures

A Wren member declares what it exposes with one attribute:

```wren
#export = "add(n: Num) -> Num"
add(n) { _score = _score + n }
```

The attribute value is a signature:

| Form | Member |
|---|---|
| `name(a: T, b) -> R` | Method, static, or constructor |
| `name -> R` | Getter |
| `name=(v: T)` | Setter |

**Rules:**

* `name` is the name Haxe sees. It can differ from the Wren name. The member is exported under it, and the runtime still calls the Wren method.
* There is one entry per Wren parameter, in order. `a` names the parameter, `a: T` also types it, and `_: T` types it while keeping the name from the source. Types match by position, so the runtime can read the same attribute from the running class.
* `-> R` types the result. Without it, the result type comes from inference.
* The attribute must match its member: the same shape, and one entry per parameter. Otherwise WrenLift refuses to compile the module, and the description fails with the same error.
* A declared `Num`, `Bool` or `String` is a contract that WrenLift enforces in every tier. A parameter is checked when the member is entered, and the result each time it returns. A value of the wrong type raises as `Fiber.abort` does, for example ``bump(_) expects Num for `x` ``. `List`, `Fn`, class types and a `Null` result are not checked, because another language hands those over in its own form.
* The attribute is optional. Without it, a member is exported under its own name with the parameter names from the source.

Type names are Wren's own names, or a class from the same module:

| In `#export` | Registry | Haxe |
|---|---|---|
| `Num` | `Float` | `Float` |
| `Bool` | `Bool` | `Bool` |
| `String` | `Str` | `String` |
| `List` | `Array(Dyn)` | `caribou.Sequence<Dynamic>` |
| `Fn` | `Fun` | `Dynamic` |
| `Fn(Num, Hud) -> Bool` | `Function` | `(Float, Hud) -> Bool` |
| `Null`, as a result | `Void` | `Void` |
| A class of the module | `Object("module.Class")` | That class |
| A class from a namespaced import, `import "swarm:Entity" for Entity` | `Object` of that class | That class: the Haxe class, or the class emitted for the Wren module |
| Anything else, or nothing | `Dyn` | `Dynamic` |

A function type lists its parameter types and its result type, each drawn from this table. `Fn()` takes no arguments, and a missing `-> Type` means `Dynamic`.

A result declared `Null` is dropped at the crossing. Declare `Null` on a method that is called only for its side effects: a Wren body returns the value of its last expression, and a `Dynamic` result boxes a number on every call.

### Inference

A result type needs no `#export` when the runtime can infer it from the body. The runtime runs WrenLift's own inference and uses:

* A literal: `count { 0 }` is `Num`, `flag { true }` is `Bool`.
* An interpolation: `"%(prefix): %(_score)"` is `String`.
* A constructor call: `Hud.new(0)` is `Hud`.
* A field whose assignments are typed, or another method's result.

Parameters are never inferred; they are only declared, and they are where inference starts. A field assigned from an untyped parameter is `Dyn`, and so is every result built from it.

### Values

| Wren | Crossing | Haxe |
|---|---|---|
| `Num` | By value | `Float` (an `Int` going into Wren becomes a `Num`) |
| `Int64`, for an integer no double holds exactly | By reference, the same integer | `haxe.Int64` |
| `Bool` | By value | `Bool` |
| `String` | By value, copied | `String` |
| `null` | | `null` |
| An object | By reference | The class emitted for it, otherwise `caribou.Ref` |
| A `List` | By reference | `caribou.Sequence<Dynamic>` over it |
| A Haxe object coming back | By reference | The same Haxe object |

* A Wren object that reaches Haxe twice is the same Haxe object. A Haxe object of an emitted class that goes into Wren is the Wren object it represents. Haxe's `==` works.
* A Wren list reaches Haxe as a `caribou.Sequence`. `xs.length`, `xs[i]`, `xs[i] = v`, and `for (x in xs)` operate on the list where Wren stores it, so both sides see one list, and it goes back as itself. `toArray()` copies it when a Haxe array is needed. A Haxe `Array<T>` is already a `Sequence<T>`, so a Haxe array can be passed where a `List` is declared, and Wren iterates over it where Haxe stores it.
* The Haxe object keeps the Wren object alive. Wren's collector sees what Haxe holds.
* A Wren abort inside a call is thrown into Haxe as a `String` containing the message. A Haxe exception that crossed into Wren and comes back is rethrown as the original exception.

## Zyntax Modules: Python and ZynML

A module of a Zyntax frontend is a module of the world like a Wren module. The Python frontend registers when a root holds a `.py` file; ZynML registers from a `zynml.zsnap` under a root. Each frontend keeps its own language's conventions, and Caribou reads them rather than choosing its own:

* **Layout:** A module is found the way its language lays modules out. `game.tally` is `game/tally.py` or `game/tally/__init__.py`.
* **Exports:** A Python module exports its top-level `def`s and `class`es: the names in `__all__` when it sets one, otherwise every name without a leading underscore. A class exports its methods without a leading underscore. A ZynML module exports what it declares public.
* **Functions belong to the module.** They are not gathered into an invented class. Wren imports them as module variables and calls each as it calls a `Fn`. Haxe imports types, so it does not see them; a class the module declares is what Haxe reaches.

```python
# src/game/tally.py
def score(hits: int, misses: int) -> int:
    return hits * 10 - misses * 3

class Tally:
    def __init__(self, hits: int):
        self.hits = hits
    def total(self, misses: int) -> int:
        return score(self.hits, misses)
```

```wren
import "game:tally" for score, Tally
System.print(score.call(7, 2))   // 64
```

```haxe
import game.tally.Tally;         // the class, under the module as its package
```

Types come from the module's declarations: Python annotations and ZynML signatures. Numbers, booleans, and strings cross; an instance of a Python or ZynML class does not cross yet, so a call that takes or returns one fails with an error that says so.

## Lua Modules

The Lua frontend registers when a root holds a `.lua` file. A Lua module is the table its chunk returns, and `game.counter` is `game/counter.lua` or `game/counter/init.lua`. A file directly under a root is a module of Lua's own namespace: `scale.lua` is `lua:scale`.

* **Functions** of the table belong to the module, as in Python.
* **Classes:** A table of functions the module holds is a class, read the way Lua writes one:
  * `new` constructs an instance;
  * a function declared with `:` is a method of an instance;
  * the fields of the tables the class is the metatable of are the instances' fields;
  * the class's other functions and fields are statics.
* **Metafields:** Keys that begin with `__` belong to the metatable and are not members.

* **Types:** LuaLS annotations, the `---@` comments the Lua language server reads, are the types the other languages see. Without them a member takes and returns dynamic values.
  * `---@param` and `---@return` type a function's parameters and result.
  * `---@class` names a class, and its `---@field`s type its instances' fields.
  * `---@type` types a static field.
  * `integer` is an `Int`, `number` a `Float`, `string`, `boolean`, a `fun(...)` type, and a class of the module by its name. Any other type, or one that may be nil, is dynamic.
  * An annotation that names a parameter the function does not have is an error when the module is described or loaded.
* **Several results:** A Lua caller keeps as many results as it asks for; a caller in another language keeps as many as the function declares. With one `---@return` or none, the call gives the first result. With several, it gives all of them, missing ones as nil, as one value:
  * Haxe gets an anonymous structure, each result a field under its `---@return` name (`_1`, `_2`, ... for an unnamed one): `var s = c.state(); s.count`.
  * Wren gets a `List` of them, in order: `c.state()[0]`.
  * Python gets a tuple: `q, r = qr(17, 5)`.
  * Lua gets that many results from a call to another language that gives several values at once: `local a, b = f()`.
* **Tuples of other languages:** A Python tuple passed to another language crosses as the same core tuple, its values named `_1`, `_2`, ...: a `List` in Wren, an anonymous structure in Haxe.

```lua
-- src/game/counter.lua
---@class Counter
---@field n integer
local Counter = {}
Counter.__index = Counter

---@type integer
Counter.LIMIT = 10

---@param start integer
---@return Counter
function Counter.new(start) return setmetatable({ n = start }, Counter) end

---@param by integer
---@return integer
function Counter:bump(by)
  self.n = math.min(self.n + by, Counter.LIMIT)
  return self.n
end

return { Counter = Counter }
```

```haxe
import game.counter.Counter;
var c = new Counter(3);
c.bump(4);                       // 7: bump(by:Int):Int
trace(c.n, Counter.LIMIT);       // 7, 10, both Int
```

```wren
import "game:counter" for Counter
var c = Counter.new(3)
System.print(c.bump(4))          // 7
System.print(Counter.LIMIT)      // 10
```

A function from Haxe or Wren can be passed to Lua and called there. A Lua function that comes back is called with `call` in Wren, and as a function in Haxe when its type is declared. An instance of the class that another member returns is an instance of it in Haxe and Wren. For how bytes cross, see [Buffers](#buffers).

## Functions & Callbacks

A function crosses by reference, keeps its captured environment, and comes back as itself.

```wren
// Wren gives Haxe functions: a typed parameter, an untyped one, and a
// static that Haxe keeps and calls later from its own code.
Player.twice(Fn.new {|x| x * 3 }, 2)
Player.apply(Fn.new {|s| s + "!" }, "hi")
Player.onHit = Fn.new {|d| System.print("hit for %(d)") }
```

```haxe
// Haxe gives Wren functions, which Wren calls like any Fn.
Hud.onTick(function(n:Dynamic):Dynamic return n * 2);
Hud.twice(function(x:Float):Float return x + 1, 1);
```

* A Wren `Fn` arrives in Haxe as a real function value: `cb(3)` calls it, `Reflect.isFunction` returns true, and a parameter or field declared `Int -> Void` accepts it. Read back by Wren, it is the same `Fn`. When the member declares the function's type, such as `adder() -> Fn(Num) -> Num`, the value is a function of that type (`Float -> Float`), and Haxe calls it as one of its own without boxing. When the member declares only `Fn`, the value is the variadic function that `Reflect.makeVarArgs` produces, and each call boxes its arguments.
* A Haxe function arrives in Wren as an object that answers `call(...)` with up to eight arguments and `arity`, like a `Fn` does. It is not a `Fn`: `cb is Fn` is false.
* An exception inside a callback propagates the same way as in any call. A Haxe throw aborts the Wren fiber with its message, and a Wren abort is thrown into Haxe as a `String`. A result that Haxe has no representation for becomes `null`.
* The callee keeps the function alive for as long as it holds it, the same as for objects.
* A Wren function belongs to its VM. If it is called on another VM, or after its VM is gone, it raises "belongs to another Wren VM" instead of running. A Haxe static that holds a Wren callback must be cleared before that VM goes away. In a project there is one VM for the program's lifetime.
* `#export = "onTick(cb: Fn)"` maps to `Dynamic` in Haxe. `cb: Fn(Num)` maps to `Float -> Void`.

## Static State

State that a class keeps for itself is shared by reference and never copied. Every access goes through the owner, so a read sees the latest write from either language, and a write lands in the owner's storage.

* A Haxe `static var` is a static field of the class. Wren reads and writes it as `Player.spawned`.
* A Wren class keeps static state behind a static getter and setter, `static count { __count }` and `static count=(v) { __count = v }`. Haxe sees these as one static property, `Hud.count`.
* A Wren module-level `var` belongs to no class. Another Wren module imports it by name. Haxe does not see it.

## Native Names

A program never writes these names, but they are the contract between the emitted Haxe class and the runtime. They appear in a `.hl` file's native table and in error messages.

Each member of an emitted class is a native of the `caribou` library named `namespace:module.Class.signature`, where the signature is the member's Wren signature:

| Member | Native Name |
|---|---|
| `add(n)` | `game:hud.Hud.add(_)` |
| `score` | `game:hud.Hud.score` |
| `score=(v)` | `game:hud.Hud.score=(_)` |
| `static best(a, b)` | `game:hud.Hud.static:best(_,_)` |
| `construct new(score)` | `game:hud.Hud.construct:new(_)` |

The name is the entire binding. Nothing needs to be published before the program starts, and a module that is published again answers the next call.

## Describing a Module

`caribou describe src/game/hud.wren` prints the module's interface as JSON: the classes, their members with kind, Wren signature, parameter names and types, and result type. `caribou describe src` prints the same for every module of every language under the root, each with its file path. This is what the Haxe library reads. It has the same shape the runtime publishes to the registry when the module loads, so the two cannot disagree.

Describing runs none of a module's code. A Lua module is described from the types the Lua compiler gives its chunk, so its top level can require a Haxe class or a plugin that only exists once the program runs. When the module runs, every function and class of the description is there with the same kind, and the running table may add fields the compiler could not see, such as those stored under computed keys.
