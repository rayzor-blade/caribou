# Wren Imports of Haxe Classes

## Overview

When Wren code writes `import "game:Player" for Player`, it imports a class that the registry holds, and the Wren adapter installs a Wren class that represents it. This page describes the mechanism behind the rules in [interop.md](../interop.md#wren-using-haxe).

## Resolution & Installation

`caribou_wren::import::configure` installs `resolve_module_fn` and `load_module_fn` ahead of any callbacks the host has set. A name of the form `ns:module`, where `ns` is a namespace the registry knows, resolves to the language's own module name, for example `haxe:game.Player`. Every namespace that addresses a given module therefore reaches the same class.

**Installation:** The first resolution installs the class. The adapter builds a WrenLift `ModuleBlob` in memory with one `ClassMir` per published class. Each class has one field and an empty top level, and nothing else. The adapter encodes the blob as a `.wlbc` and passes it to `interpret_bytecode`, the same path a `.wlbc` file takes.

**Native binding:** The adapter then binds one native per member into the class's method table:

* `new(_)` for the constructor.
* `hit(_)` for a method.
* `hp` and `hp=(_)` for a field.
* Static methods under `static:`.
* `static:spawned` and `static:spawned=(_)` for a static field. These read and write the interface's class object through the protocol.

WrenLift binds a class's foreign stubs only by `dlsym` in a `#!native` library and never consults `bind_foreign_method_fn`, so the binding happens here, immediately after installation. A namespaced name that no interface answers is left to the VM, whose import error names it.

## Call Path

Each member is bound as a host native (`bind_host`): one entry function for every member, plus a word that WrenLift passes to the entry on each call. The word is the member's target, which the binding keeps for as long as the class exists. It contains the member's kind, its callable, its name for the trace, and the call site that the callee's protocol fills. A call therefore performs no lookup. The entry makes the bridge call that the kind names:

* `call_at` with the typed callable, for a method or a static.
* `get_at` and `set_at` by interned symbol, for a field.
* The constructor's callable, for `new`.

**Argument passing:** A method or static called with only scalar arguments takes the direct send its site holds (`bridge::call_direct_at`, see [call sites](bridge.md#call-sites)). A scalar already has the bridge value's layout, so the arguments cross where they are and nothing needs to be rooted. Any other call, and the first call before the site is filled, copies its arguments into a stack buffer sized for Wren's widest signature, with a slot for the receiver in front. The arguments cross as bridge values; a Wren string becomes a core `Str`, rooted for the duration of the call. Results come back through `to_wren`, so an object from another language is held as an instance of the class installed for its type, and that class's module is installed on first need.

**Errors and inheritance:** A Haxe throw, or an argument that Haxe rejects, arrives as the error's message and aborts the fiber, which `Fiber.try` can catch. A Wren class can extend an installed class. Its constructor's `super` call reaches the same native, with the instance already created.

## Lifetime

A Haxe object that enters Wren is held through a cell (`caribou::cell`, see [bridge.md](bridge.md#cells--shadows)): the one core object that represents it. The adapter creates the cell on the first crossing and finds it by the object's address afterward, because a Haxe object keeps no shadow.

**Views:** The cell keeps a view for Wren 16 bytes in, at the offset where WrenLift's prefix places an object's header. The view is an `ObjInstance` of the class installed for the object's type, with no fields, which `proxy` writes. Wren holds the cell through that view, so a send on it is WrenLift's own dispatch with the cell as the receiver, and `foreign_of` reads the cell back as the object it represents. The same object crossing twice is the same Wren value, including under `==`. A Haxe function or array is held the same way, under the `Function` or `Sequence` class. An object that is already a cell created by another language gets its view in that existing cell.

**Collection:** A cell is not an allocation of the Wren heap, so the heap keeps a list of the cells that Wren holds through their views (`hold_view`), and flags each cell in its bridge word. The anchor keeps them alive outside a cycle, as it keeps every pin. WrenLift's marking marks a cell in that word the same way it marks its own objects. At the end of a cycle, the adapter hands the marked cells to the anchor to mark in the core collection that ends the cycle. It does not claim them, because a claim marks an object without tracing it, and only the cell's trace keeps the object the cell holds. The unmarked cells leave the list, and the core's collection decides whether they survive. A held cell counts as pressure toward the next cycle, and handing out a view is a safepoint, like an allocation, so a Wren program that allocates nothing of its own still collects what it drops. A scan of a native range finds a held cell alongside the heap's own objects.

**Subclass adoption:** A Wren class can extend an installed class. Instances of such a class are allocated on the Wren heap with one hidden field, `__caribou_object`, that holds the address of the cell of the object the constructor adopted. The instance is marked as adopted in its bridge word, so the heap's trace marks the cell, and the cell's trace marks both the object and the instance in front of it. The cell lives as long as the instance, the object comes back as the instance, and when the instance dies the cell no longer has a front.
