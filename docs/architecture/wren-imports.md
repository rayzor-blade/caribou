# Wren imports a Haxe class

`import "game:Player" for Player` in Wren reaches a class the registry
holds, and the Wren adapter installs a Wren class that stands for it. This
page is the mechanism behind the rules in
[interop.md](../interop.md#wren-using-haxe).

## Resolving and installing

`caribou_wren::import::configure` installs `resolve_module_fn` and
`load_module_fn` ahead of any the host set. A name `ns:module` under a
namespace the registry knows resolves to the language's own module name,
`haxe:game.Player`. So every namespace addressing one module reaches one
class.

The first resolution installs it. The adapter builds a wren_lift
`ModuleBlob` in memory: one `ClassMir` per published class, with one
field, an empty top level and nothing else. It encodes the blob as a
`.wlbc` and hands it to `interpret_bytecode`, the path a `.wlbc` file
takes.

Then it binds one native per member into the class's method table:

- `new(_)` for the constructor;
- `hit(_)` for a method;
- `hp` and `hp=(_)` for a field;
- statics under `static:`;
- `static:spawned` and `static:spawned=(_)` for a static field, which read
  and write the interface's class object through the protocol.

wren_lift binds a class's foreign stubs only by `dlsym` in a `#!native`
library and never consults `bind_foreign_method_fn`, so the binding
happens here, right after the install. A namespaced name no interface
answers is left to the VM, whose import error names it.

## A call

Each member is bound as a host native, `bind_host`: one entry for every
member, and a word wren_lift hands the entry on each call. The word is
the member's target, which the binding keeps for as long as the class:
its kind, its callable, its name for the trace, and the call site the
callee's protocol fills. So a call looks nothing up. It is the bridge
call the kind names:

- `call_at` with the typed callable, for a method or a static;
- `get_at` and `set_at` by interned symbol, for a field;
- the constructor's callable, for `new`.

A method or a static called with scalars alone takes the direct send its
site holds (`bridge::call_direct_at`, see [call
sites](bridge.md#call-sites)). A scalar has the bridge value's layout, so
the arguments cross where they lie, and nothing is rooted. Any other
call, and the first one, before the site is filled, crosses its
arguments into a buffer on the stack, sized for Wren's widest signature,
with a slot before them for the receiver. They cross as bridge values; a
Wren string becomes a core `Str`, rooted for the call. Results come back
through `to_wren`, so an object of another language becomes an instance
of the class installed for its type, installing that class's module on
first need.

A Haxe throw, or an argument Haxe refuses, arrives as the error's message
and aborts the fiber, which `Fiber.try` sees. A Wren class may extend an
installed one; its constructor's `super` call reaches the same native with
the instance already made.

## Lifetime

An instance of an installed class is an ordinary `ObjInstance` with two
hidden fields, held as numbers. The first is the address of the object
it stands for: the `HaxeRef` the bridge wraps a Haxe object in. The
instance is marked adopted in its bridge word, and the heap's trace
marks what an adopted instance holds, so that object lives for as long
as the Wren instance does.

The instance is the object's stand-in, and there is one per object: the
VM's imports keep a map from the object to its instance, keyed by the
native object behind the ref (a Haxe object wrapped twice is one key),
and a crossing finds the instance there before making one. The second
field is that key, so the cycle can remove the entry when the instance
dies without reading the object, which may be dead too. The map does
not root the instance, so an entry means a live instance. Going the
other way, an instance leaving Wren, as an argument or a result, is the
object it stands for (`from_wren`).
