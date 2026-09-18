# Haxe imports a Wren class

`import game.hud.Hud` in Haxe names a class the build macro declared from
a Wren file, and every member of it is a native the Ash adapter binds when
the program loads. This page is the mechanism behind the rules in
[interop.md](../interop.md#haxe-using-wren).

## The build macro

Haxe types every call at compile time, and the HL target refuses `extern
class`. So a Wren class Haxe code uses has to be declared before genhl
runs. That declaration is the build macro's whole job.

`haxe/` is the `caribou` haxelib, and `-lib caribou` is all a program
adds. The library's extra params run `caribou.Bridge.use()`
(`haxe/caribou/Bridge.hx`). The macro walks the program's classpath for
`.wren` files and emits a class for each class they define, under the
package the file's path spells, as for a Haxe module. `src/game/hud.wren`
gives `game.hud.Hud`: `game` is the namespace the runtime resolves the
module through, and `hud` is the module's name. A file at a classpath root
is under the language's own namespace.

What the macro knows about a module, it asks the runtime. `caribou
describe` prints the module's interface as JSON, a
`caribou::describe::ModuleDesc`. That is the registry `Interface` without
its callables, plus the parameter names a source has, produced from the
parse tree by the same rules the publisher applies to the running VM. The
command is found on the path, else in the target directory of the
checkout the library sits in.

## Declaring types

Wren declares no types. So a member says what it exposes in one of
wren_lift's attributes, which reach the MIR and the class at run time:

```wren
#export = "add(n: Num) -> Num"
add(n) { _score = _score + n }
```

The value is the member's exported signature. The name is what other
languages see, and it may differ from Wren's. There is one parameter per
Wren parameter, each `name`, `name: Type` or `_: Type`, where `_` keeps
the source's name. `-> Type` gives the result. A getter is `name ->
Type`; a setter is `name=(v: Type)`. Types are Wren's own names, a
class of the module, a class a namespaced import brings in by the name
it is imported as, or a function's shape, `Fn(Num) -> Num`
(`caribou_wren::types`). An imported class's registry name is not known
from the source, so `describe` writes it as the import and the class,
`swarm:Entity.Entity`, which the macro resolves to the Haxe class or the
class it emitted for that Wren module; the publisher resolves the same
name on the running VM, where the import is a class object it knows.

Parameters match by position. So the publisher can read the attribute
off the running class, which has no parameter names, the way `describe`
reads it from source, and the published interface is typed and named as
the description is. The attribute is optional, and it is checked against
its member from source: the wrong shape or arity refuses the description.

A member without one is exported under Wren's name with the source's
parameter names. Its parameters are `Dyn`, and its result is what
wren_lift's own inference gives: `describe_source` runs the three-pass
inference the LSP hover runs, and takes the result of a literal, an
interpolation, a constructor call, a field or another method from it.
Parameters are only ever declared; they are where the inference starts.

## The emitted class

An emitted class extends the emitted class of its Wren superclass when
that is a class of the same module, so `Panel is Hud` is `Panel extends
Hud` and a Panel is a Hud to Haxe's types; else it extends
`caribou.Ref`, whose one field holds the object's ref (below): a
superclass from elsewhere, a Haxe class or another module's, has no
emitted class to extend. Every member of it is a native of the `caribou`
library, named for what it reaches: `game:hud.Hud.add(_)`, which is the
namespace, the module, the class and the member's Wren signature, with
`static:` before a static's and `construct:` before the constructor's.

The native is a static taking the receiver, since genhl emits nothing for
`@:hlNative` on an instance method, and an inline method or property
wraps it with the declared types. The parameters carry the declared
types, so a number crosses as itself and nothing is boxed on the way in.
A `List` is `caribou.Sequence<Dynamic>`, an abstract over either a Haxe
array or a foreign sequence behind its ref, whose `length`, `[]` and
`[]=` reach the sequence through three natives of the library's own,
`len`, `index` and `set_index`, which the binder recognises by name; a
Haxe array is answered in Haxe, without a crossing.
The result is `Float` or `Bool` when the member declares one and
`Dynamic` otherwise: a number comes back in a register, and an object
comes back boxed and is cast by the wrapper, since the cast is what
checks its class.

The constructor calls its native with the fresh Haxe object, and the
native makes the Wren object and binds the two. A named constructor is a
static factory. A static getter is a static property.

A subclass declares its own instance members and no more: a member it
overrides is reached through the superclass's wrapper, since the native
sends to the object and Wren's own dispatch finds the override. Its
statics it declares with its superclasses' of the module, since Haxe
does not inherit statics and Wren does; each is sent to the subclass's
own class object, where Wren finds the inherited one. A Haxe constructor
must call its parent's, and a Wren subclass's constructor is its own, so
the `super()` of a subclass that constructs itself passes placeholders,
and the superclass's native binds nothing to an instance of a subclass
with a constructor of its own. A subclass without one inherits the
constructor, in Haxe as in Wren: the superclass's native constructs the
subclass through it, sending the signature to the subclass's class
object. Both natives tell the case by the fresh object's own class.

Two Wren members of one name and different arities cannot both be one
Haxe method: the second declared is spelled with its arity appended,
`draw1`, and one of a name a superclass declares the same way. A static
and an instance member of one name clash the same way.

## Binding the natives

When the program loads, `caribou_ash::import::bind` reads its natives.
Each `caribou` one is parsed into a slot: namespace, module, class,
member, kind. The slot keeps the kinds the program declared the native
with, and is registered with ash's resolver as the one entry, called by
record with the slot's address as its context (`native_lib::HostNative`).
Ash's interpreter, Cranelift tier and LLVM tier write the typed
arguments as one word each into a record on the stack and call
`entry(slot, record)`; the entry reads each word by the slot's kinds, and
answers with one word the tier reads by the declared result kind. So one
entry serves every signature, and a native can be declared with whatever
types the member has.

Binding is by name alone. Nothing has to be published before the program
starts, ash looks for no library, and the call finds the member when it
happens: through the registry for a static or a constructor, and through
the object for the rest. So a class published again answers the next
call. A slot keeps what a static or a constructor resolved to, under the
registry generation it resolved in, replaced whole when something
publishes. It also keeps a `CallSite` the callee's protocol fills. So a
call after the first does no lookup on either side.

The arguments cross as bridge values. The result comes back through the
Haxe conversion. A bridge error is thrown into Haxe as the exception it
carries, else as its message in a `String`. `attach_types` records, once
the interpreter has built its types, which Haxe class each slot's class
is, and `caribou.Ref`, for the cells below, and the program's `String`
for the strings that cross.

## Cells

A Wren object reaching Haxe is a cell (`caribou::cell`): the one core
object standing for it in Haxe's terms. Word zero is a descriptor whose
`hl_type` prefix mirrors the class the program declares for the
object's published type, found through the registry by the object's
`type_name`, else `caribou.Ref` when the program has that class. To
HashLink the cell is an instance of that class: it dispatches, casts
and tests the type through the mirror, which shares the class's runtime
data (`hlp_get_obj_rt` is built for the class before the mirror is
made, so the mirror never builds it). Nothing else of the cell is read
by Haxe; a class the macro emits declares no field, so the cell's own
words are its own. So the same Wren object reaching Haxe twice is the
same Haxe object, `==` included, and one going back to Wren is the
object it stands for.

An instance Haxe constructs itself, `new Hud(3)`, is a real object of
the class whose first field, `hl.Abstract<"caribou_obj">`, holds the
cell: the cell keeps it in front, so the object always comes back as
that instance. A cell Haxe holds only as such a pointer, a callback's
target too, is made under a plain view, an abstract type, and is read
under the class's view from the first time Haxe gets it as an object.

## How a Wren object is held by Haxe

A Wren object reaching Haxe must be something Haxe can keep, that Haxe's
collector sees, and that stays alive in Wren for as long as Haxe keeps it.

`caribou_ash::wrap_foreign` answers with the cell under the plain view:
a traced core object of Haxe's language holding the object's bridge
value. Going the other way, a Haxe object crosses as itself, a core
object by the descriptor the core answers for a bare `hl_type` at word
zero; a language that must hold a header of its own before an object
keeps a cell for it (see [wren-imports.md](wren-imports.md#lifetime)). The name is the common case; whatever is not Haxe's is held the
same way, a core `Str` or `Error` included, and a Haxe value or a scalar
passes through. `wrenref_as_abstract` gives the cell as the raw pointer
a constructed instance's field holds, `wrenref_from_abstract` turns it
back into a value, and `unwrap_foreign` into the object.

The cell's protocol forwards every message to the object, call sites
included, so a Haxe caller reaching it through `Dynamic` gets Wren
semantics, and `equals` sees through a cell on either side. It answers
`unwrap_native` with the object itself, which is how the Wren adapter
recognises one of its own objects coming back and restores identity.

The cell's trace hook is what keeps the object. A Wren cycle ends with a
core collection, and an object Wren's own marking did not reach lives if
the core's mark reaches it, which it does through a live cell (see
[hosted collectors](heap.md#hosted-collectors)). So a cell Haxe still
holds keeps its object, and one Haxe dropped lets it go, in the same
cycle. The core's collection retains every Wren object regardless
outside a cycle.

There is one cell per object, and the object keeps it: the cell is the
protocol's *shadow* of the object for Haxe (see
[bridge.md](bridge.md#shadows)), a word on a Wren object. An object whose
language keeps no shadow, a core `Error` say, is entered in a map from
its address to the cell's instead. Neither roots the cell, so a cell is
reachable only from Haxe and dies when Haxe drops it. Its drop hook, run
by the core's sweep before the cell's lines can be reused, drops the
shadow or the entry.

A Wren VM may go before Haxe lets go of one of its objects: an isolate
ending, a VM a host drops. At its teardown (`heap_drop`) the adapter
severs every cell kept as a shadow on one of the VM's objects
(`cell::sever`): the cell stands for nothing from then on, its trace
marks nothing, and every send through it raises, "the object is gone",
where it would have reached freed memory. Unwrapped, it is the gone
object, a core object with the same answers, so it stays that whoever
holds it next. The VM's published modules leave the registry at the
same time (`registry::withdraw`), so nothing reaches its classes after;
a later lookup asks the loader again. An adopted instance in front of a
Haxe object's cell leaves the cell as it goes.
