# Zyntax 

Zyntax is a multi-language compiler infrastructure providing a unified intermediate pipeline for diverse language frontends. The core engine comprises a typed Abstract Syntax Tree (AST), a High-Level Intermediate Representation (HIR), a tiered JIT compiler, and an embedded runtime.

Host-level integration is driven by `caribou-zyntax`. Rather than forcing frontends to implement bespoke execution engines, Caribou abstracts the common surface where these languages intersect: it ingests frontends as declared modules and publishes them directly into the runtime's shared execution context.

## Frontend Abstraction & Module Discovery

A frontend implements the `Language` provider contract. This abstraction encapsulates language metadata, the module layout (Zyntax's `ModuleArchitecture`, so Python is `game/tally.py` or `game/tally/__init__.py` and a grammar language is a file per module), the module's export rules, parser implementations (whether grammar-driven or ad-hoc), and the dependencies required to link against the embedded runtime, including snapshots, plugins, and entry points. Conventions belong to the frontend: the Python frontend supplies Python's (`zyntax_python::exports`, `class_exports`: `__all__`, leading underscores), and Caribou reads them rather than choosing its own.

The compiler driver discovers frontends dynamically by scanning project root directories:

* **Registration Markers:** Placing a language snapshot (`zynml.zsnap`) or a grammar file (`lang.zyn`) directly in a root registers that language with the runtime. Python needs no marker: a `.py` file under a root registers the Python frontend (`caribou-python`), which parses with its own parser.
* **Unified Namespaces:** Source files discovered under registered roots resolve to common namespace identifiers across supported runtimes. For example, `src/game/scorer.zynml` resolves to `game:scorer` in Wren and `game.scorer` in Haxe.
* **Plugins:** Language plugins are distributed as Zyntax `.zrtl` binaries and reside in the project-relative `plugins/` directory alongside host Caribou plugins.

To the adapter layer, bare `.zyn` grammars, composite snapshots (such as ZynML and its bundled libraries), and custom external parsers (such as the Python frontend) are uniform: the engine requires only that each frontend emit a compliant typed AST and HIR.

## Module Ingestion Pipeline

When the module registry resolves an identifier such as `game/scorer`, compilation flows through a five-stage pipeline:

* **Resolution:** The language loader locates the target source file by matching registered file extensions under project roots.
* **Signature Parsing:** Source code is parsed via `parse_with_signatures`. This binds plugin signatures against application call sites and initializes the module's type registry.
* **HIR Lowering:** The typed AST is lowered to HIR inside the runtime context via `TieredRuntime::lower_to_hir`. This pass applies the active grammars, loads snapshot modules, resolves imports, and executes Krio optimization passes. A module's own imports (`import util`, `import game.util`) resolve through the resolver the adapter registers on each language's runtime: a dotted path names the module from the root; a bare name is a module of the importing module's namespace, else of any namespace of the world. Staged bundle sources come before the roots, as for the module itself. The runtime asks its snapshot's modules first, so a library name the snapshot ships wins.
* **Publishing:** The compiler introspects declarations to expose language constructs to the host registry.
* **JIT Compilation:** The lowered HIR is compiled to machine code via `compile_module`.

## Symbol & Type Publishing

Publishing derives its metadata from two distinct compiler structures: the typed AST and the lowered HIR.

**Structural Declarations (Typed AST):**
The typed AST defines identities, boundaries, and type hierarchies:

* **Exports:** Only what the frontend says the module exports is published (`Language::exports`, `exported_members`). By default that is every public function, struct, or class the module's own file declares; a frontend with rules of its own overrides it.
* **Classes and Structs:** Export member fields, instance methods, static methods, and inherent `impl` blocks. A frontend that lowers methods to functions named `Class$method` with the instance first (the Python frontend) has them attached to the class. Constructors are mapped from a static `new` returning the declaring instance type.
* **Module-Level Functions:** Functions declared outside a class scope are published as the module's own (`Interface::functions`), never gathered into an invented class. Wren imports them as module variables (`import "game:tally" for score`); Haxe imports types, so it sees only the module's classes, under the module as their package (`game.tally.Tally`).
* **Type Registry Mapping:** Scalar numbers map to `Int` or `Float`; booleans and characters/strings map to `bool` and `Str`; explicit user classes map to fully qualified `Object` identifiers (e.g., `zynml.Point`); and arrays, functions, and optionals map to `Array`, `Fun`, and wrapped inner types, respectively.

**Calling Conventions (HIR):**
The HIR provides symbol metadata and machine-level representations for Foreign Function Interface (FFI) dispatch:

* Method symbols adopt lowered naming conventions (such as `Point$len`).
* At the C ABI boundary, certain high-level types decay to identical primitives (for instance, both `String` and object instances decay to raw pointers). The engine uses the typed AST signature to disambiguate pointer classifications before generating calls.

## Native Call Dispatch

Executable targets are wrapped as `Callable::Typed` instances containing the compiled machine code address and an `hl_type` signature derived from the ABI boundary types. Foreign function calls are dispatched through Caribou's native invocation bridge (`caribou::native`):

* **Scalars:** Passed directly by machine kind.
* **Strings:** Converted to Zyntax's own string (the ZRTL string, a 16-byte header then the bytes) through `zyntax_embed::ZyntaxString`, allocated as Zyntax allocates its strings. Return values are read in place and copied into standard host-managed strings.
* **Dynamic values:** A parameter or result of Zyntax's `Any` type (an unannotated Python parameter) is the program's dynamic value. None, booleans, numbers and strings cross as the program's own; any other value of the core crosses as a foreign object (see below).
* **Complex Types:** Zyntax's own objects, arrays and function references are restricted at the call boundary. Their type signatures publish, but a call fails at dispatch with an error naming the parameter.

## Other Languages from Zyntax Programs

A Lua or Python program reaches the rest of the world through Zyntax's foreign objects (`zyntax_embed::foreign`). The adapter installs itself as the embedder once per process.

* **Imports:** Lua's `require("haxe.ScaleValues")` and Python's `from haxe.ScaleValues import ScaleValues` import the world's module `haxe:ScaleValues`. The first dotted segment is the namespace; the rest is the module, with its dots kept, as Haxe names modules. Each frontend asks the embedder only for a module it does not have itself: Lua after `package.path`, Python for a module that is neither the program's own nor a standard one.
* **Members:** A module's members are its classes and functions. A class's are its statics and static methods, and calling the class constructs one. An object's are the fields and getters its language answers, and the methods its published class declares.
* **Methods:** A method of an object is called through its class's target, which takes the receiver first, and through the object's protocol when the class does not publish it. Read as a value, a method takes its receiver first, so Lua's `o:m()` is the read and the call.
* **Lifetime:** A foreign object roots its core object with a handle, which the program's release of the box drops.
* **Errors:** An error the bridge returns is raised in the program as a library error: `TypeError`, `IndexError`, `AttributeError` or `RuntimeError` by the error's kind, with its message.

A language whose modules run, as Python's do, names the function that runs a module's body (`Language::entry`). Loading the module runs it once, after compiling, so the names its imports bind are set before any of its functions is called.

## Haxe Integration

Build macros inspect the host environment via `caribou describe <root>`. This tool outputs the structure of both native Wren source files and published Zyntax modules with their associated namespace paths. The compiler emits declarations such as `game.scorer.Scorer` directly against these structures, ensuring compile-time Haxe definitions mirror the symbols bound dynamically at runtime.

Describing a module runs none of its code. `caribou_zyntax::describe` loads each module into a world whose loader reads interfaces from source alone:

* A module that declares its exports is parsed and lowered, and published from its declarations with no code behind them. Nothing is compiled, and a Python module's body does not run.
* A Lua module is described from the types its chunk gets when `load` compiles it (`zyntax_lua::exports`). Its top level may require modules of other languages that are not there yet, since nothing runs it.

## Memory Architecture & Runtime Constraints

Zyntax manages dynamic memory using size-class allocation pools. Automatic memory management relies on compile-time drop analysis for deterministic cleanup, backed by a conservative mark-sweep garbage collector for remaining allocations.

Under Caribou, the conservative mark-sweep collector is intentionally disabled:

* **Thread Model Incompatibility:** The Zyntax collector assumes a single mutator thread with predictable thread-local stack boundaries. This assumption conflicts with Caribou's task-scheduled runtime architecture.
* **Current Lifecycle Behavior:** Allocations that cannot be proven dead by static drop analysis—including strings passed across call boundaries—remain allocated for the duration of the execution context.
* **Future Work:** The target memory architecture requires migrating dynamic allocations to Caribou's host heap. Under this design, allocated blocks receive host descriptor words, allowing escape analysis and garbage collection tracing to be handled entirely by the host core.

## Current Implementation Boundaries

**Implemented:**

* Dynamic frontend discovery for `.zyn` grammars and precompiled snapshot bundles.
* Typed AST and HIR publication pipeline for structs, classes, and module-level functions, filtered by the frontend's export rules.
* The Python frontend as a language: layout, exports, classes, and module functions.
* The Lua frontend as a language, embedded as a C host embeds Lua: the state opened once (`zyntax_lua::open_host`), and each `?.lua` or `?/init.lua` module loaded with Lua's own `load` and run with a protected call through the C API's cores, so an error in the chunk is the load's. A file directly under a root is a module of Lua's own namespace, `lua:scale` for `scale.lua`, as Wren's are. The module's interface is the table its chunk returns, as the Lua compiler's types know it (`zyntax_lua::exports`). Its functions are the module's, each with its parameters, and each of its tables that holds functions is a class, read the way Lua writes one:

* `new` is the constructor. When it is declared with `:`, as in `Account:new(o)`, it is called with the class as its receiver.
* A function declared with `:` (one taking `self` first) is a method, called with an instance as its receiver.
* Any other function is a static method, and any other field a static field.
* The fields of the tables whose metatable the class is (`setmetatable(t, Counter)`) are its instances' fields.
* Keys that begin with `__` are the metatable's own and are not members.
* The types are the LuaLS annotations the frontend registers with what each annotates (`zyntax_lua::LuaType`), mapped to the core's types; a member without them is `Dyn`.
* A table whose metatable is a published class reports that class's type name, so an instance another member returns crosses as the class's instance.
* A function whose annotations declare several results is published with a `Tuple` result, named by its `@return`s. Its call asks Lua for that many results and gives them as one core tuple (`caribou::data::tuple_new`), which Haxe takes as an anonymous object and Wren as a `List`.
* Zyntax carries several values as one tuple box. A core tuple that a call or method gives a Zyntax program is that many values (`Foreign::values`, which the library's `zb_foreign_call` and `zb_foreign_invoke` expand): several results in Lua, a tuple in Python. A tuple box that crosses out is a core tuple, its values named `_1`, `_2`, ...

Haxe writes `new Counter(3)`, `c.bump(4)`, `c.n` and `Counter.LIMIT`, and calls `lua.scale.Scale.run(device, queue)`; Wren writes the same with its own syntax. Running the chunk gives the same names the same kinds, reads their values from the table, and adds the fields only the running table shows, such as those stored under keys the compiler cannot see. A Lua table or function that crosses to another language is a core object (`lua.table`, `lua.function`) answering reads, writes, method calls (the receiver first) and calls through protected calls, and comes back to Lua as itself. Lua's strings are its bytes, and a buffer is read in place (see [interop.md](../interop.md#buffers)). A proxy keeps its Lua value in the registry, counted, until the last proxy of it dies.
* Native C ABI call dispatch for scalar types and managed strings.
* Wren and Haxe cross-language module resolution and metadata generation.
* Other languages' modules, classes, objects and plugins in Python programs, as foreign objects; dynamic parameters and results across calls into Zyntax.
* Distribution within a Caribou bundle: each frontend as a language section (its snapshot, or a name for one built into Caribou), its modules as source (see [bundle.md](bundle.md)).
* Reload of an edited module through the runtime's own hot reload, with the interface published again (see [world.md](world.md#reload)).

**Pending Architecture:**

* Host-heap memory integration and unified garbage collection.
* Object, array, and closure passing across the native FFI boundary (mapping Zyntax instances to host core objects via `TypeMeta` and `TypeDesc`).
* A Lua module another Lua module requires from the world arrives as the world's module, whose functions call back into Lua, rather than as the table its chunk returned.
* Python's typed externs: other languages' classes with their declared signatures, checked when the program compiles.
* Awaiting a core future from Python.
* Effect system and fiber synchronization across the native runtime bridge.
* Bundled modules in Zyntax's compiled form: the snapshot's lowered HIR with declarations beside it, in place of source.