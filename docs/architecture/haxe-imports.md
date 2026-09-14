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
class of the module, or a function's shape, `Fn(Num) -> Num`
(`caribou_wren::types`).

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

Every emitted class extends `caribou.Ref`, whose one field holds the
object's ref (below). Every member of it is a native of the `caribou`
library, named for what it reaches: `game:hud.Hud.add(_)`, which is the
namespace, the module, the class and the member's Wren signature, with
`static:` before a static's and `construct:` before the constructor's.

The native is a static taking the receiver, since genhl emits nothing for
`@:hlNative` on an instance method, and an inline method or property
wraps it with the declared types. The parameters carry the declared
types, so a number crosses as itself and nothing is boxed on the way in.
The result is `Float` or `Bool` when the member declares one and
`Dynamic` otherwise: a number comes back in a register, and an object
comes back boxed and is cast by the wrapper, since the cast is what
checks its class.

The constructor calls its native with the fresh Haxe object, and the
native makes the Wren object and binds the two. A named constructor is a
static factory. A static getter is a static property. A subclass declares
what it inherits from a superclass of the same module and stands alone
under `caribou.Ref`, because a Haxe constructor must call its parent's,
and a Wren subclass's constructor is its own.

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
is, and `caribou.Ref`, for the faces below, and the program's `String` for
the strings that cross.

## Faces

A Haxe object of an emitted class is the face of one foreign object. Its
first field holds the object's ref, and the ref knows its face. So the
same Wren object reaching Haxe twice is the same Haxe object, and a face
going back to Wren is the object it stands for.

A foreign object crossing into a `Dynamic` gets a face under the class
the program declares for its published type, found through the registry
by the object's `type_name`, else under `caribou.Ref` when the program has
that class. The ref and its face hold each other, and both die when Haxe
lets go.

## How a Wren object is held by Haxe

A Wren object reaching Haxe must be something Haxe can keep, that Haxe's
collector sees, and that stays alive in Wren for as long as Haxe keeps it.

`caribou_ash::wrap_foreign` answers with a `WrenRef`: a traced core object
under a static descriptor of Haxe's language, holding the object's bridge
value. The name is the common case; whatever is not Haxe's is wrapped the
same way, a core `Str` or `Error` included, and a Haxe value or a scalar
passes through. Haxe keeps the ref as the raw pointer
`wrenref_as_abstract` gives, in the `hl.Abstract<"caribou_obj">` field of
the class the build macro emits: a word the conservative scan sees and
HashLink never reads. `wrenref_from_abstract` turns it back into a value,
and `unwrap_foreign` into the object.

The ref's protocol forwards every message to the object, call sites
included, so a Haxe caller reaching it through `Dynamic` gets Wren
semantics, and `equals` sees through a ref on either side. It answers
`unwrap_native` with the object itself, which is how the Wren adapter
recognises one of its own objects coming back and restores identity.

The ref's trace hook is what keeps the object. A Wren cycle ends with a
core collection, and an object Wren's own marking did not reach lives if
the core's mark reaches it, which it does through a live ref (see [hosted
collectors](heap.md#hosted-collectors)). So a ref Haxe still holds keeps
its object, and one Haxe dropped lets it go, in the same cycle. The core's
collection retains every Wren object regardless outside a cycle.

There is one ref per object, and the object keeps it: the ref is the
protocol's *shadow* of the object for Haxe (see
[bridge.md](bridge.md#shadows)), a word on a Wren object. An object whose
language keeps no shadow, a core `Error` say, is entered in a map from
its address to the ref's instead. Neither roots the ref, so a ref is
reachable only from Haxe and dies when Haxe drops it. Its drop hook, run
by the core's sweep before the ref's lines can be reused, drops the
shadow or the entry.

The invariant is that a ref an object keeps, or the map names, is alive.
Presence means alive, because the only way out is the drop of the ref
itself, and the object cannot die before its ref, whose trace marks it.
So `wrap_foreign` on an object that already has a ref returns that ref,
and a second wrap of one object is the same ref until Haxe lets it go.
Once it does, the object dies with the next Wren cycle that finds no other
reference to it.
