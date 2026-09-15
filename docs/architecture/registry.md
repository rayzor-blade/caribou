# Module registry

`caribou::registry` is where one language's classes become visible to
another. An adapter that loads a module publishes its interface. The
adapter answering another language's import reads that interface and
installs a class of its own that targets the bridge. Nothing is generated.
Each compiler binds the imported class where it binds its own.

## Interfaces

An `Interface` describes one module of one language: its `lang`, its
`module` name in that language's own terms (`game.Player` for Haxe), and
its classes.

A `ClassIface` has:

- the class's simple name;
- its `type_name`, what the language calls the type, and what an instance
  reports through the protocol's `type_name` message;
- its superclass;
- its fields, with their types;
- its static fields, and a `class_object` they are read and written on;
- its methods and its constructor.

A `MethodIface` has a name, a static flag, parameter and return types, and
the `Callable` the bridge invokes for it. An instance method's callable
takes the receiver first. A static's takes only its parameters. A
constructor's takes the constructor's parameters and returns the new
object. Types are `TypeRef`s: `Void`, `Bool`, `Int`, `Float`, `Str`,
`Object(type name)`, `Array`, `Dyn` and `Fun`, the terms every language
can map to.

`publish(iface)` puts an interface in the process-wide table, replacing an
earlier one of the same `(lang, module)`, and bumps a generation counter
a caller can cache against; a reload publishes the module again (see
[world.md](world.md#reload)). `interface(lang, module)` reads one back.
`class_for_type(lang, type_name)` finds the class an object belongs to.
`lookup(namespace, module)` and `lookup_class` resolve an import path
first. A language may register a loader with `set_loader`;
`resolve_or_load` and `lookup_class_or_load` ask the namespace's loaders
in turn when nothing has published a module yet, which is how a module
loads on first use. A loader that read a module from a file records the
file with `set_source`, and `sources` lists them, for the world's watch.

## Namespaces

An import addresses a module through a namespace, not a language name.
`import "game:Player"` names the namespace `game` and the module `Player`.

`World::new` publishes `Config.namespaces` process-wide, the way it
publishes language names, so an adapter callback with no world handle can
resolve one. A `Namespace` has a name, the languages it covers and, when
given, the modules it exposes.

A module is addressable in a namespace by its own name. When its name
begins with the namespace's name and a dot, it is also addressable by the
remainder. That is how the Haxe package `game` becomes the namespace
`game`, and `game:Player` reaches `game.Player`. A Wren module in a
directory is addressable with `/` as well, `game:ui/hud`. Every registered
language is also a namespace under its own name, so `haxe:game.Player`
resolves with no configuration.

`publish` refuses an interface when a configured namespace holds its
language and another, and both would answer one import name with a module
of their own. The error is a `RegisterError`.

## What Ash publishes

`caribou_ash::program` loads a `.hl` the way ash's CLI does. The runner
and the tests share it. `load` installs the seam, initialises ash's
standard library, decodes the bytecode and builds the interpreter. `start`
runs the entry point, which is HashLink's entry function: it creates every
class object and runs the static initialisers before `main`. The
interpreter registers its closure runner, stub resolver and exception
hooks only then, so nothing in a program can be called from outside
before it has started. `publish` walks the decoded types.

HashLink's shape is this. An instance type (`game.Player`) carries the
fields and the instance methods as protos. Its companion (`game.$Player`,
an `hl.Class`) carries the statics as function-typed fields, bound by its
binding list to their functions, and binds the inherited `__constructor__`
field to the constructor.

Every published callable is a `Callable::Cell`: the address of the
function's entry in the module context's `functions_ptrs`, and its
function type. The entry holds a stub sentinel that `hlp_dyn_call` routes
to the closure runner, or the compiled entry once the tier has promoted
the function. Reading it per call is how a caller follows the promotion.
The interpreter keeps that context private, so it is read off the type of
a `String` the program allocates through its own `String.__alloc__`, and
that type is kept for the strings that cross.

A constructor is published as a `Callable::Dynamic`: a small core object
whose `call` allocates an instance of the type with `hlp_alloc_obj`, wraps
it, and runs `__constructor__` from its cell on it through the dispatcher.
So the registry stays free of anything Haxe.

The companion's own unbound fields are the class's static fields. The
class's `class_object` is a core object naming the instance type. It finds
the `hl.Class` instance in the type's global at each use, since the entry
function makes it after the program publishes, and its `get_member` and
`set_member` reach the static fields through the Haxe protocol on that
instance.

Types under `hl.` and `haxe.`, the companions and `String` are not
published. There is one module per class, named after it.

## What WrenLift publishes

`caribou_wren::publish_module(vm, "hud")` publishes the classes a loaded
Wren module defines, under Wren's language and the module's own name.
Another language reaches them as `wren:hud`, or through a configured
namespace as `game:hud`.

Each class is described from what the VM built for it.

- Its name, and its superclass unless that is Object.
- Its fields: the names in the VM's layout for the class, inherited ones
  included, all `Dyn`.
- Its members, from its method table. The table is a copy of the
  superclass's plus the class's own, so an entry the class defines is one
  that differs from the superclass's at the same slot. The entries are
  Wren signatures: `draw()`, `hit(_)`, `score` for a getter, `score=(_)`
  for a setter, and under `static:` the class's own side, where a
  constructor is `static:new(_)`.
- A getter or setter is published as a `MethodIface` whose `kind()` says
  so, read from the signature its callable carries, since Wren's getters
  stand where Haxe has fields.
- A member's name and types come from its `#export` attribute (see
  [declaring types](haxe-imports.md#declaring-types)); else it is Wren's
  name and `Dyn`.
- Operators and subscripts have no name an importer can spell, and are not
  published.

A class belongs to the module when one of its own methods was compiled in
it. That leaves out what the module imported and what the adapter
installed for another language. One constructor is the class's `ctor`,
`new` when there is one; any other is a static method returning the
class. The type name an instance reports is `hud.Hud`, kept on the heap
record so the protocol's `type_name` can answer it.

Every member's target is a `Callable::WrenMethod`: the class as a core
value, the signature as a core symbol, and whether the class or the first
argument receives it. The bridge turns it into an `invoke` of that
signature through the object protocol. So a call from Haxe is the call
Wren code would make, dispatched by wren_lift itself, as
[adapters.md](adapters.md#dispatch) describes. The class stays valid while
its module does, and the VM must be entered on the calling thread, as for
any message to a Wren object.
