# Module Registry & Namespaces

## Overview

`caribou::registry` is how one language's classes become visible to another. When an adapter loads a module, it publishes the module's interface to the registry. When another language imports that module, its adapter reads the interface and installs a class of its own that calls into the bridge. No code is generated. Each compiler binds the imported class in the same place it binds its own classes.

## Interfaces

An `Interface` describes one module of one language: its `lang`, its `module` name in that language's own terms (for example `game.Player` for Haxe), its classes, and the functions the module itself owns. A language whose modules export functions (Python, ZynML) publishes them as `functions`, each a `MethodIface` that takes only its parameters. Each importing language maps them in its own way: Wren installs a module variable per function, and Haxe, which imports types, does not see them.

A `ClassIface` contains:

* The class's simple name.
* Its `type_name`: the name the language uses for the type, and the name an instance reports through the protocol's `type_name` message.
* Its superclass.
* Its fields, with their types.
* Its static fields, plus a `class_object` that the static fields are read from and written to.
* Its methods and its constructor.

A `MethodIface` contains a name, a static flag, the parameter and return types, and the `Callable` the bridge invokes for it. An instance method's callable takes the receiver as its first argument. A static method's callable takes only its parameters. A constructor's callable takes the constructor's parameters and returns the new object. Types are `TypeRef` values: `Void`, `Bool`, `Int`, `Float`, `Str`, `Object(type name)`, `Array`, `Dyn`, and `Fun`. These are the types that every language can map to its own.

**Registry API:**

* `publish(iface)` stores an interface in the process-wide table, replacing any earlier interface with the same `(lang, module)`, and bumps a generation counter that callers can cache against. A reload publishes the module again (see [world.md](world.md#reload)).
* `interface(lang, module)` returns a published interface.
* `withdraw(lang, module)` removes an interface and the source file it came from. A runtime calls this for its modules when it shuts down, so that nothing can reach its callables afterward.
* `class_for_type(lang, type_name)` finds the class that an object belongs to.
* `lookup(namespace, module)` and `lookup_class` resolve an import path first, then look up the interface.
* `set_loader` registers a loader for a language. `resolve_or_load` and `lookup_class_or_load` try the namespace's loaders in turn when no module has been published yet. This is how a module loads on first use. A loader that read a module from a file records the file with `set_source`, and `sources` lists those files for the world's file watcher.

## Namespaces

An import addresses a module through a namespace, not through a language name. `import "game:Player"` names the namespace `game` and the module `Player`.

`World::new` publishes `Config.namespaces` process-wide, the same way it publishes language names, so an adapter callback without a world handle can still resolve a namespace. A `Namespace` has a name, the languages it covers, and optionally the modules it exposes.

**Resolution rules:**

* A module is addressable in a namespace by its own name.
* If the module's name starts with the namespace's name followed by a dot, the module is also addressable by the remainder. This is how the Haxe package `game` becomes the namespace `game`, and `game:Player` reaches `game.Player`.
* A Wren module in a subdirectory is addressable with `/`, for example `game:ui/hud`.
* Every registered language is also a namespace under its own name, with no configuration. `haxe:game.Player` always resolves.

`publish` rejects an interface when a configured namespace covers its language and another language, and both would answer the same import name with a module of their own. The error is a `RegisterError`.

## What Ash Publishes

`caribou_ash::program` loads a `.hl` file the same way Ash's CLI does. The runner and the tests share this code.

* `load` installs the seam, initializes Ash's standard library, decodes the bytecode, and builds the interpreter.
* `start` runs the entry point, which is HashLink's entry function. The entry function creates every class object and runs the static initializers before `main`. The interpreter registers its closure runner, stub resolver, and exception hooks only at this point, so nothing in a program can be called from outside before the program has started.
* `publish` walks the decoded types and publishes them.

**HashLink type layout:** An instance type such as `game.Player` carries the fields and the instance methods as protos. Its companion type, `game.$Player` (an `hl.Class`), carries the static methods as function-typed fields, bound to their functions through its binding list, and binds the inherited `__constructor__` field to the constructor.

**Callables:** Every published callable is a `Callable::Cell`: the address of the function's entry in the module context's `functions_ptrs`, plus its function type. The entry holds either a stub sentinel, which `hlp_dyn_call` routes to the closure runner, or the compiled entry once the tier has promoted the function. Reading the entry on each call is how a caller follows the promotion. The interpreter keeps the module context private, so the adapter reads it from the type of a `String` that the program allocates through its own `String.__alloc__`, and keeps that type for the strings that cross.

**Constructors:** A constructor is published as a `Callable::Dynamic`: a small core object whose `call` allocates an instance of the type with `hlp_alloc_obj`, wraps it, and runs `__constructor__` on it from its cell through the dispatcher. This keeps the registry free of anything specific to Haxe.

**Static fields:** The companion type's own unbound fields are the class's static fields. The class's `class_object` is a core object that names the instance type. On each use it finds the `hl.Class` instance in the type's global, because the entry function creates that instance after the program publishes. Its `get_member` and `set_member` reach the static fields through the Haxe protocol on that instance.

Types under `hl.` and `haxe.`, the companion types, and `String` are not published. There is one module per class, named after the class.

## What WrenLift Publishes

`caribou_wren::publish_module(vm, "hud")` publishes the classes that a loaded Wren module defines, under the Wren language and the module's own name. Another language reaches them as `wren:hud`, or through a configured namespace as `game:hud`.

Each class is described from what the VM built for it:

* **Name and superclass:** The class's name, and its superclass unless the superclass is `Object`.
* **Fields:** The field names in the VM's layout for the class, including inherited fields. All fields are `Dyn`.
* **Members:** Taken from the class's method table. The table is a copy of the superclass's table plus the class's own methods, so a method the class defines is one whose entry differs from the superclass's entry at the same slot. Entries are Wren signatures: `draw()`, `hit(_)`, `score` for a getter, `score=(_)` for a setter, and, under `static:`, the class's static side, where a constructor is `static:new(_)`.
* **Getters and setters:** Published as a `MethodIface` whose `kind()` reports the getter or setter kind, read from the signature its callable carries. Wren getters take the place that fields have in Haxe.
* **Names and types:** A member's name and types come from its `#export` attribute (see [declaring types](haxe-imports.md#declaring-types)). Without an attribute, the name is Wren's name and the types are `Dyn`.
* **Operators and subscripts:** These have no name an importer can spell, so they are not published.

**Module membership:** A class belongs to the module if one of its own methods was compiled in that module. This excludes classes the module imported and classes the adapter installed for another language. One constructor becomes the class's `ctor`: `new` if there is one. Any other constructor is published as a static method that returns the class. The type name an instance reports is `hud.Hud`. The adapter stores it on the heap record so that the protocol's `type_name` can return it.

**Targets:** Every member's target is a `Callable::WrenMethod`: the class as a core value, the signature as a core symbol, and a flag that says whether the class or the first argument receives the call. The bridge turns it into an `invoke` of that signature through the object protocol. A call from Haxe is therefore the same call Wren code would make, dispatched by WrenLift itself, as [adapters.md](adapters.md#dispatch) describes. The class stays valid as long as its module does, and the VM must be entered on the calling thread, as for any message to a Wren object.
