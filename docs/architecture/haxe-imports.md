# Haxe Imports of Wren Classes

## Overview

When Haxe code writes `import game.hud.Hud`, it imports a class that the build macro declared from a Wren file. Every member of that class is a native that the Ash adapter binds when the program loads. This page describes the mechanism behind the rules in [interop.md](../interop.md#haxe-using-wren).

## The Build Macro

Haxe types every call at compile time, and the HL target does not allow `extern class`. A Wren class that Haxe code uses therefore has to be declared before genhl runs. Producing that declaration is the build macro's job.

`haxe/` is the `caribou` haxelib, and `-lib caribou` is the only thing a program adds. The library's extra params run `caribou.Bridge.use()` (`haxe/caribou/Bridge.hx`). The macro describes each classpath root and emits one Haxe class for each class the other languages' modules define. The file's path determines the package, as it does for a Haxe module:

* `src/game/hud.wren` produces `game.hud.Hud`. `game` is the namespace the runtime resolves the module through, and `hud` is the module's name.
* A file at a classpath root goes under the language's own namespace.

**Describing modules:** The macro asks the runtime for everything it knows about a module. `caribou describe` prints the module's interface as JSON, a `caribou::describe::ModuleDesc`. This is the registry `Interface` without its callables, plus the parameter names found in the source. The description is produced from the parse tree by the same rules the publisher applies to the running VM. The macro looks for the `caribou` command on the path, and otherwise in the target directory of the checkout that contains the library.

## Declaring Types

Wren has no type declarations, so a member states what it exposes in one of WrenLift's attributes. Attributes are available in the MIR and on the class at run time:

```wren
#export = "add(n: Num) -> Num"
add(n) { _score = _score + n }
```

**Signature syntax:**

* The attribute value is the member's exported signature. The name is what other languages see, and it may differ from the Wren name.
* There is one parameter entry per Wren parameter. Each entry is `name`, `name: Type`, or `_: Type`, where `_` keeps the name from the source. `-> Type` declares the result.
* A getter is written `name -> Type`. A setter is written `name=(v: Type)`.
* Types are Wren's own type names, a class of the same module, a class that a namespaced import brings in (under the name it is imported as), or a function shape such as `Fn(Num) -> Num` (see `caribou_wren::types`).
* The registry name of an imported class is not known from the source alone, so `describe` writes it as the import plus the class, for example `swarm:Entity.Entity`. The macro resolves this to the Haxe class, or to the class it emitted for that Wren module. The publisher resolves the same name on the running VM, where the import is a class object it knows.

**Positional matching:** Parameters match by position. This lets the publisher read the attribute from the running class, which has no parameter names, the same way `describe` reads it from the source, so the published interface has the same names and types as the description. The attribute is optional. When present, it is checked against its member in the source: a wrong shape or arity makes the description fail.

**Inference:** A member without an attribute is exported under its Wren name with the parameter names from the source. Its parameters are `Dyn`. Its result type is whatever WrenLift's own inference produces: `describe_source` runs the same three-pass inference that the LSP hover uses, and takes the result type of a literal, an interpolation, a constructor call, a field, or another method. Parameters are never inferred; they are where inference starts.

**Plugins and Zyntax:** The macro emits a plugin's classes the same way, from the libraries in `plugins/` next to the compiler output: one module per class, under the plugin's name as the package, for example `math.Vec2` (see [plugins.md](plugins.md)). It emits a Zyntax module's classes from what the module publishes (see [zyntax.md](zyntax.md)).

## The Emitted Class

**Inheritance:** An emitted class extends the emitted class of its Wren superclass when the superclass is in the same module. `Panel is Hud` becomes `Panel extends Hud`, so a Panel is a Hud to Haxe's type system. Otherwise the class extends `caribou.Ref`, whose single field holds the object's ref (see below). A superclass from elsewhere, whether a Haxe class or a class from another module, has no emitted class to extend.

**Natives:** Every member is a native of the `caribou` library, named after what it reaches: `game:hud.Hud.add(_)`, which is the namespace, the module, the class, and the member's Wren signature. A static member's name has `static:` in front, and the constructor's has `construct:`. The native is declared as a static function that takes the receiver as its first argument, because genhl emits nothing for `@:hlNative` on an instance method. An inline method or property wraps the native with the declared types.

**Types:**

* Parameters carry the declared types, so a number crosses as a number and nothing is boxed on the way in.
* A `List` becomes `caribou.Sequence<Dynamic>`, an abstract over either a Haxe array or a foreign sequence behind its ref. Its `length`, `[]`, and `[]=` reach the sequence through three natives of the library's own (`len`, `index`, and `set_index`), which the binder recognizes by name. A Haxe array is handled in Haxe without a crossing.
* The result type is `Float` or `Bool` when the member declares one, and `Dynamic` otherwise. A number comes back in a register. An object comes back boxed, and the wrapper casts it, because the cast is what checks its class.

**Constructors:** The constructor calls its native with the newly created Haxe object. The native creates the Wren object and binds the two together. A named constructor becomes a static factory method. A static getter becomes a static property.

**Subclasses:** A subclass declares only its own instance members. A member it overrides is reached through the superclass's wrapper, because the native sends to the object and Wren's own dispatch finds the override. The subclass declares its own statics together with the statics of its superclasses in the same module, because Haxe does not inherit statics and Wren does. Each static is sent to the subclass's own class object, where Wren finds the inherited one. A Haxe constructor must call its parent's constructor, but a Wren subclass's constructor stands on its own. The `super()` call of a subclass that has its own constructor therefore passes placeholder arguments, and the superclass's native binds nothing when it sees an instance of a subclass that has its own constructor. A subclass without a constructor inherits it, in Haxe as in Wren: the superclass's native constructs the subclass through it, sending the signature to the subclass's class object. Both natives tell the two cases apart by the class of the new object.

**Name collisions:** Two Wren members with the same name and different arities cannot both become one Haxe method. The second one declared gets its arity appended to its name, for example `draw1`. The same applies to a member whose name a superclass already declares this way, and to a static and an instance member with the same name.

## Binding the Natives

When the program loads, `caribou_ash::import::bind` reads its natives. It parses each `caribou` native into a slot: namespace, module, class, member, and kind. The slot keeps the kinds the program declared the native with, and is registered with Ash's resolver as the single entry function, called by record with the slot's address as its context (`native_lib::HostNative`).

**Record calls:** Ash's interpreter, its Cranelift tier, and its LLVM tier all write the typed arguments as one word each into a record on the stack and call `entry(slot, record)`. The entry reads each word according to the slot's kinds and returns one word, which the tier reads according to the declared result kind. One entry therefore serves every signature, and a native can be declared with whatever types the member has.

**Binding by name:** Binding is by name only. Nothing needs to be published before the program starts, Ash does not look for a library, and the call finds the member when it happens: through the registry for a static or a constructor, and through the object for everything else. A class that is published again answers the next call. A slot caches what a static or a constructor resolved to, tagged with the registry generation it resolved in, and replaces the whole cache when something publishes. It also keeps a `CallSite` that the callee's protocol fills. A call after the first one therefore performs no lookup on either side.

**Conversions:** Arguments cross as bridge values. The result comes back through the Haxe conversion. A bridge error is thrown into Haxe as the exception it carries, or as its message in a `String` when it carries none. Once the interpreter has built its types, `attach_types` records which Haxe class each slot's class corresponds to, along with `caribou.Ref` for the cells described below and the program's `String` type for strings that cross.

## Cells

A Wren object that reaches Haxe is a cell (`caribou::cell`): the one core object that represents it in Haxe's terms. Word zero of the cell is a descriptor whose `hl_type` prefix mirrors the class the program declares for the object's published type. The adapter finds that class through the registry by the object's `type_name`, and falls back to `caribou.Ref` when the program has that class. To HashLink, the cell is an instance of that class: it dispatches, casts, and type-checks through the mirror, which shares the class's runtime data. (`hlp_get_obj_rt` is built for the class before the mirror is created, so the mirror never builds it.) Haxe reads nothing else from the cell. A class the macro emits declares no fields, so the cell's own words remain its own. The same Wren object reaching Haxe twice is therefore the same Haxe object, including under `==`, and an object that goes back to Wren is the object it represents.

**Constructed instances:** An instance that Haxe constructs itself, with `new Hud(3)`, is a real object of the class. Its first field, `hl.Abstract<"caribou_obj">`, holds the cell. The cell keeps the instance in front of it, so the object always comes back as that instance. A cell that Haxe holds only as such a pointer, including a callback's target, is created under a plain view (an abstract type) and is read under the class's view from the first time Haxe receives it as an object.

## How Haxe Holds Wren Objects

A Wren object that reaches Haxe has to be something Haxe can store, that Haxe's collector can see, and that stays alive in Wren for as long as Haxe holds it.

**Wrapping:** `caribou_ash::wrap_foreign` returns the cell under the plain view: a traced core object of the Haxe language that holds the object's bridge value. In the other direction, a Haxe object crosses as itself, because it is already a core object under the descriptor the core returns for a bare `hl_type` at word zero. A language that needs its own header in front of an object keeps a cell for it instead (see [wren-imports.md](wren-imports.md#lifetime)). Wren objects are the common case, but anything that is not a Haxe value is held the same way, including a core `Str` or `Error`. Haxe values and scalars pass through unchanged. `wrenref_as_abstract` returns the cell as the raw pointer that a constructed instance's field holds, `wrenref_from_abstract` turns that pointer back into a value, and `unwrap_foreign` turns it into the object.

**Forwarding:** The cell's protocol forwards every message to the object, including call sites. A Haxe caller that reaches the cell through `Dynamic` therefore gets Wren semantics, and `equals` sees through a cell on either side. The cell answers `unwrap_native` with the object itself, which is how the Wren adapter recognizes one of its own objects coming back and restores its identity.

**Liveness:** The cell's trace hook is what keeps the object alive. A Wren collection cycle ends with a core collection, and an object that Wren's own marking did not reach survives if the core's mark reaches it. The core's mark reaches it through a live cell (see [hosted collectors](heap.md#hosted-collectors)). A cell that Haxe still holds therefore keeps its object alive, and a cell that Haxe has dropped lets the object go in the same cycle. Outside a cycle, the core's collection keeps every Wren object alive regardless.

**Identity:** There is one cell per object, and the object stores it. The cell is the protocol's *shadow* of the object for Haxe (see [bridge.md](bridge.md#cells--shadows)), stored in a word on the Wren object. An object whose language keeps no shadow, such as a core `Error`, is entered in a map from its address to the cell's instead. Neither the shadow nor the map roots the cell, so a cell is reachable only from Haxe and dies when Haxe drops it. Its drop hook, which the core's sweep runs before the cell's lines can be reused, removes the shadow or the map entry.

**Severed cells:** A Wren VM may go away before Haxe releases one of its objects, for example when an isolate ends or a host drops a VM. During teardown (`heap_drop`), the adapter severs every cell that is stored as a shadow on one of the VM's objects (`cell::sever`). From then on the cell represents nothing: its trace marks nothing, and every send through it raises "the object is gone" instead of touching freed memory. Unwrapped, it is the gone object, a core object that gives the same answers, so it stays that way for whoever holds it next. The VM's published modules leave the registry at the same time (`registry::withdraw`), so nothing can reach its classes afterward; a later lookup asks the loader again. An adopted instance in front of a Haxe object's cell leaves the cell when the VM goes.
