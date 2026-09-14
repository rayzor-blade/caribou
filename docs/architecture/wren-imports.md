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
through `to_wren`, so an object of another language is held as an
instance of the class installed for its type, installing that class's
module on first need.

A Haxe throw, or an argument Haxe refuses, arrives as the error's message
and aborts the fiber, which `Fiber.try` sees. A Wren class may extend an
installed one; its constructor's `super` call reaches the same native with
the instance already made.

## Lifetime

A Haxe object entering Wren is held through a cell (`caribou::cell`,
see [bridge.md](bridge.md#cells-and-shadows)): the one core object
standing for it, which this adapter makes on the first crossing and
finds by the object's address after, since a Haxe object keeps no
shadow. The cell keeps a view for Wren 16 bytes in, where wren_lift's
prefix puts an object's header: an `ObjInstance` of the class installed
for the object's type, with no fields, which `proxy` writes. Wren holds
the cell through that view, so a send on it is wren_lift's own dispatch,
its receiver the cell, which `foreign_of` reads back as the object it
stands for. The same object crossing twice is the same Wren value, `==`
included, and a Haxe function or array is held the same way, under the
`Function` or `Sequence` class. An object that is a cell already, of
another language's making, gets its view in that cell.

A cell is no allocation of this heap, so the heap keeps a list of the
cells Wren holds through their views (`hold_view`), flagged in the
cell's bridge word, and the anchor retains them outside a cycle as it
retains every pin. wren_lift's marking marks a cell in that word as it
marks its own objects; a cycle claims the marked ones for the core, whose
trace keeps the object each holds, and drops the rest from the list, for
the core's collection to decide. A held cell counts as pressure toward
the next cycle, and handing a view out is a safepoint, as an allocation
is, so a Wren program that allocates nothing of its own still collects
what it drops. A scan of a native range finds a held cell beside the
heap's own objects.

A Wren class may extend an installed one. Its instances are the heap's
own, with one hidden field, `__caribou_object`, holding the address of
the object the constructor adopted, and the instance is marked adopted
in its bridge word, so the heap's trace marks what it holds. The
object's cell keeps such an instance in front, and the object comes
back as it; when the instance dies, the cell has no front.
