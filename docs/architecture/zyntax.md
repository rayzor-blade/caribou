# Zyntax 

Zyntax is a multi-language compiler infrastructure providing a unified intermediate pipeline for diverse language frontends. The core engine comprises a typed Abstract Syntax Tree (AST), a High-Level Intermediate Representation (HIR), a tiered JIT compiler, and an embedded runtime.

Host-level integration is driven by `caribou-zyntax`. Rather than forcing frontends to implement bespoke execution engines, Caribou abstracts the common surface where these languages intersect: it ingests frontends as declared modules and publishes them directly into the runtime's shared execution context.

## Frontend Abstraction & Module Discovery

A frontend implements the `Language` provider contract. This abstraction encapsulates language metadata, recognized file extensions, parser implementations (whether grammar-driven or ad-hoc), and the dependencies required to link against the embedded runtime, including snapshots, plugins, and entry points.

The compiler driver discovers frontends dynamically by scanning project root directories:

* **Registration Markers:** Placing a language snapshot (`zynml.zsnap`) or a grammar file (`lang.zyn`) directly in a root registers that language with the runtime.
* **Unified Namespaces:** Source files discovered under registered roots resolve to common namespace identifiers across supported runtimes. For example, `src/game/scorer.zynml` resolves to `game:scorer` in Wren and `game.scorer` in Haxe.
* **Plugins:** Language plugins are distributed as Zyntax `.zrtl` binaries and reside in the project-relative `plugins/` directory alongside host Caribou plugins.

To the adapter layer, bare `.zyn` grammars, composite snapshots (such as ZynML and its bundled libraries), and custom external parsers (such as the Python frontend) are uniform: the engine requires only that each frontend emit a compliant typed AST and HIR.

## Module Ingestion Pipeline

When the module registry resolves an identifier such as `game/scorer`, compilation flows through a five-stage pipeline:

* **Resolution:** The language loader locates the target source file by matching registered file extensions under project roots.
* **Signature Parsing:** Source code is parsed via `parse_with_signatures`. This binds plugin signatures against application call sites and initializes the module's type registry.
* **HIR Lowering:** The typed AST is lowered to HIR inside the runtime context via `TieredRuntime::lower_to_hir`. This pass applies the active grammars, loads snapshot modules, resolves imports, and executes Krio optimization passes.
* **Publishing:** The compiler introspects declarations to expose language constructs to the host registry.
* **JIT Compilation:** The lowered HIR is compiled to machine code via `compile_module`.

## Symbol & Type Publishing

Publishing derives its metadata from two distinct compiler structures: the typed AST and the lowered HIR.

**Structural Declarations (Typed AST):**
The typed AST defines identities, boundaries, and type hierarchies:

* **Classes and Structs:** Export member fields, instance methods, static methods, and inherent `impl` blocks. Constructors are mapped as static `new` methods returning the declaring instance type.
* **Module-Level Functions:** Functions declared outside a class scope are aggregated and published as static members on a synthetic class named after the module (e.g., free functions in `scorer` map to static members of `Scorer`).
* **Type Registry Mapping:** Scalar numbers map to `Int` or `Float`; booleans and characters/strings map to `bool` and `Str`; explicit user classes map to fully qualified `Object` identifiers (e.g., `zynml.Point`); and arrays, functions, and optionals map to `Array`, `Fun`, and wrapped inner types, respectively.

**Calling Conventions (HIR):**
The HIR provides symbol metadata and machine-level representations for Foreign Function Interface (FFI) dispatch:

* Method symbols adopt lowered naming conventions (such as `Point$len`).
* At the C ABI boundary, certain high-level types decay to identical primitives (for instance, both `String` and object instances decay to raw pointers). The engine uses the typed AST signature to disambiguate pointer classifications before generating calls.

## Native Call Dispatch

Executable targets are wrapped as `Callable::Typed` instances containing the compiled machine code address and an `hl_type` signature derived from the ABI boundary types. Foreign function calls are dispatched through Caribou's native invocation bridge (`caribou::native`):

* **Scalars:** Passed directly by machine kind.
* **Strings:** Converted to Zyntax's native length-prefixed string representation, backed by memory allocated from Zyntax's size-class pool. Return values are copied into standard host-managed strings.
* **Complex Types:** Objects, arrays, and function references are currently restricted at the call boundary. While their type signatures publish correctly, invocation attempts fail at dispatch time with an explicit parameter rejection error.

## Haxe Integration

Build macros inspect the host environment via `caribou describe <root>`. This tool outputs the structure of both native Wren source files and published Zyntax modules with their associated namespace paths. The compiler emits declarations such as `game.scorer.Scorer` directly against these structures, ensuring compile-time Haxe definitions mirror the symbols bound dynamically at runtime.

## Memory Architecture & Runtime Constraints

Zyntax manages dynamic memory using size-class allocation pools. Automatic memory management relies on compile-time drop analysis for deterministic cleanup, backed by a conservative mark-sweep garbage collector for remaining allocations.

Under Caribou, the conservative mark-sweep collector is intentionally disabled:

* **Thread Model Incompatibility:** The Zyntax collector assumes a single mutator thread with predictable thread-local stack boundaries. This assumption conflicts with Caribou's task-scheduled runtime architecture.
* **Current Lifecycle Behavior:** Allocations that cannot be proven dead by static drop analysis—including strings passed across call boundaries—remain allocated for the duration of the execution context.
* **Future Work:** The target memory architecture requires migrating dynamic allocations to Caribou's host heap. Under this design, allocated blocks receive host descriptor words, allowing escape analysis and garbage collection tracing to be handled entirely by the host core.

## Current Implementation Boundaries

**Implemented:**

* Dynamic frontend discovery for `.zyn` grammars and precompiled snapshot bundles.
* Typed AST and HIR publication pipeline for structs, classes, synthetic module classes, and free functions.
* Native C ABI call dispatch for scalar types and managed strings.
* Wren and Haxe cross-language module resolution and metadata generation.

**Pending Architecture:**

* Host-heap memory integration and unified garbage collection.
* Object, array, and closure passing across the native FFI boundary (mapping Zyntax instances to host core objects via `TypeMeta` and `TypeDesc`).
* Bi-directional import resolution allowing Zyntax modules to import external host languages.
* Effect system and fiber synchronization across the native runtime bridge.
* Driver-managed discovery for frontends utilizing independent, non-grammar parsers (such as Python).
* Standalone distribution of Zyntax modules within composite deployment bundles.