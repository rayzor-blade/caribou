# Object Protocol & Call Bridge

## Overview

The bridge is the part of the core that a value passes through when it moves from one language to another. It consists of these modules:

* `caribou::protocol` defines the messages that every heap object can answer.
* `caribou::symbol` interns names.
* `caribou::error` defines the error value.
* `caribou::bridge` performs the call.
* `caribou::diag` formats the report that a driver prints.

Each adapter provides its side of the contract: a protocol implementation for its own objects and, in Ash's case, a typed dispatcher. [adapters.md](adapters.md) describes how Haxe and Wren objects implement the protocol.

## Values at the Boundary

Two ABIs meet at every cross-language call. Typed HashLink code passes raw scalars and pointers according to a signature. Dynamically typed runtimes pass `caribou_abi::Value`, a single NaN-boxed word. The core converts between the two representations once, at the boundary, and never inside a language.

* **Descriptor contract:** An object `Value` that reaches the bridge must have a `TypeDesc` at word zero. An adapter whose native objects carry a bare `hl_type` wraps them before they cross.
* **Value representation:** A `Value` holds an `i32`, a double, a bool, null, or an object pointer.
* **Boxed values:** A value that a language has but a `Value` word cannot hold crosses as a core object. A string crosses as a `Str`. A 64-bit integer outside the `i32` range crosses as an `Int64` (defined in `caribou::error`). A language with 64-bit integers reads the box back exactly; a language with only doubles receives the nearest double.

## The Protocol

Every heap object answers a fixed set of messages through the `Protocol` vtable that its `TypeDesc` points to. An adapter implements the vtable once for its own types. The core dispatches through the vtable whenever a value crosses into a language that did not create it.

| Message | Meaning |
|---|---|
| `get_member`, `set_member` | Read or write a named field or property, identified by an interned symbol |
| `invoke` | Call a named member with arguments |
| `get_member_at`, `set_member_at`, `invoke_at` | The same operations, through a call site that the caller keeps (see below). Optional |
| `call` | Call the object itself, if it is callable |
| `arity` | The number of arguments that `call` takes |
| `index`, `set_index`, `len`, `iterate` | Sequence and map access |
| `to_string`, `hash`, `equals` | Identity and display |
| `unwrap_native` | The native payload of a plugin object |
| `type_name` | The name the registry publishes the object's class under, as an interned symbol |
| `shadow`, `keep_shadow`, `drop_shadow` | The object from another language that represents this one, stored on the object (see below). Optional |
| `is_error`, `error_message`, `error_kind`, `error_cause`, `error_trace` | The error protocol |

**Reply codes:**

* A message the object does not support returns `Unsupported`. The calling language maps this to its own "missing member" error.
* An entry that raises an error stores the error as pending on the current task and returns `Raised`.
* Entries use the `extern "C-unwind"` ABI. A panic inside a Rust entry unwinds to the bridge's protected boundary instead of aborting inside the entry.

## Symbols

`caribou::symbol` provides one interner per process. `intern` returns a `Symbol` for a name, and `name` returns the name for a `Symbol`. The interner leaks the strings, because a symbol lives for the lifetime of the process.

**Concurrency:** Interning takes a lock; reading does not. Entries live in chunks that are allocated once and never move, indexed by id. The interner hands out an id only after it has written the entry and published the table length past it. Reading `name` or `hash` costs two atomic loads and an index, which is cheap enough for a protocol entry on a hot path.

**Hashing:** Each symbol also stores `hash`, which is HashLink's field hash of the name. The hash uses the same loop as Ash's `hlp_hash_gen`: over the UTF-16 code units, `h = 223 * h + unit` in wrapping 32-bit arithmetic, followed by a truncating remainder by `0x1FFFFF7B`. A name therefore hashes to the same value that `hashed_name` holds for it in Ash. HashLink's own table also probes upward when two live names collide; that behavior depends on its cache and is not reproduced here.

## Errors

`Error` is a heap object that belongs to the core's own language, `LANG_CORE`. It is a `KIND_DYNAMIC | TRACED` allocation under a static `TypeDesc`. Its trace hook marks its fields. Its protocol answers the five error messages, `to_string`, and `get_member` for `kind`, `message`, `cause`, `native`, `origin`, and `trace`.

An `Error` has these fields:

* The kind, one of `caribou_abi::ErrorKind`.
* The language that raised it.
* A message, stored as a core `Str`. A `Str` is UTF-8 bytes after a two-word header. It is traced with no children, so the collector never scans the bytes.
* An optional cause.
* An optional native payload. This is the originating language's own error object. The bridge keeps it so that a round trip unwraps back to it.
* A trace, stored as a core `Trace`. A `Trace` is a fixed-capacity list of frames that is replaced with a larger copy when it fills up.

**Trace frames:** A frame records the language that was crossed, the callable's name, and, when known, the source it was in (a file path or module name, as a core `Str`) and a byte span into that source. `push_frame` adds a frame with a source and span. `push_segment` adds a frame with neither, which is what the bridge does at a language boundary.

**Constructors:** `with_native` builds a `User` error around a native payload. `from_value` checks a value's descriptor to tell whether it is an `Error`.

**Rooting:** Nothing in this module is boxed; every object is accessed through a raw pointer or a `Value`. A NaN-boxed `Value` on the stack is invisible to the conservative scanner. The module therefore roots every object it creates with a handle, from allocation until the object is stored in a rooted parent or returned to the caller, and it roots every object it holds across an allocation.

## The Pending Error

An error leaves a language as a pending value, not as an unwinding exception. `bridge::set_pending` stores the error for the current task, `take_pending` returns it and clears the slot, and `has_pending` checks whether one is set.

The slot is a thread-local map keyed by task id rather than a `HostState`. A task has one host-state slot, and that slot belongs to the adapter that spawned the task. A task never leaves the world that created it, so the thread-local map holds exactly one slot per task in this world. The slot roots its value through a handle while it waits, because the map itself is not a heap root. A task that finishes without its error being taken leaves an entry behind; when the map grows past a small threshold, the next insert sweeps such entries out.

## Typed Dispatch

A typed callable is a C function pointer with an `hl_type_fun` signature and the id of the language it belongs to. `Callable::Typed` stores the pointer directly. `Callable::Cell` stores the address of a cell and reads the pointer from the cell on each call. Ash publishes functions as cells because its `functions_ptrs` entry is where the tier installs a promoted body.

The bridge does not marshal such a call itself. Each language registers a `TypedDispatch` with `set_typed_dispatch`. The dispatcher is a C-ABI function that receives the function pointer, the signature, the caller's call site (if any), the arguments as `Value`s, and an out slot, and returns a reply code. The bridge reads the dispatcher from a table of atomics indexed by language id.

* **Ash's dispatcher** calls compiled code directly under Ash's own id. When the pointer is real code rather than one of the interpreter's stub sentinels, and the arity matches, the dispatcher places the arguments by the signature's kinds through `ash_native_call`, without boxing, under a HashLink trap. A stub, an arity mismatch, or a kind the direct path does not handle falls back to `hlp_dyn_call`, the same path a dynamic call takes in Ash. Calls in the other direction, from Haxe into the bridge, go through a host native called by record; see [haxe-imports.md](haxe-imports.md#binding-the-natives).
* **The core's default dispatcher**, registered for `LANG_CORE`, covers what the core's own callables and the tests need: up to four arguments of kinds `HI32`, `HBOOL`, `HF64`, and `HDYN`, returning `HVOID`, `HI32`, `HBOOL`, `HF64`, or `HDYN`. It works by transmuting the function pointer to the matching C function type. On the native ABIs the core runs on, integer-class arguments of any width share a register or stack slot, so one table over two classes (integer, float) covers every signature. Wasm types every function exactly, so an `i32` argument and an `i64` argument are different function types; there the table has one arm per exact type, over three classes (`i32`, `i64`, `f64`) and four result widths.

## Calls

`bridge::call(callable, args, caller)` invokes a callable on behalf of the language `caller`:

* A **dynamic callable** receives the `call` message.
* A **typed callable** is checked for arity against its signature and passed to its language's dispatcher.
* A **Wren method** (`Callable::WrenMethod`, which holds a class, a Wren signature such as `hit(_)`, and a static flag) becomes an `invoke` of that signature on the first argument, or on the class for a static method or a constructor. The call reaches the Wren protocol like any other message.

`call_named` does the same and records the callee's name in the trace. `invoke`, `get`, and `set` send `invoke`, `get_member`, and `set_member` with the same handling.

Every crossing provides three guarantees:

* **Protection.** The dispatch runs under `catch_unwind`. A panic becomes an `Internal` error that carries the panic message. An entry that returned `Unsupported` or `Missing` becomes a `Type` or `Runtime` error that names the value and the member. An entry that returned `Raised` yields the pending error. A pending value that is not an `Error` is wrapped in one as its native payload, so it still unwraps correctly when it returns home.
* **Trace extension.** Every error that leaves a call gains one trace frame: the callee's language, its name if the caller supplied one (`<callable>` otherwise), and no source.
* **Round-trip unwrapping.** A value that returns to the language that raised it is unwrapped. When the error has a native payload and its origin is `caller`, the call returns the payload instead of the `Error`. A Haxe exception that passed through Wren and back is therefore the same Haxe object.

## Call Sites

A caller that makes the same send from one location keeps a `CallSite` for it and uses `invoke_at`, `get_at`, `set_at`, or `call_at`. A site is three words. The callee's protocol fills them with whatever it derived for that site, under a key of its own choosing, and reads them back when the key matches. A site that sees the same shape again performs no lookup. The protocol entries `invoke_at`, `get_member_at`, and `set_member_at` are optional; the bridge falls back to the plain entries when they are missing.

**Keys:**

* Wren keys a site by its VM and stores the interned signature symbols in it. After that, finding the method is an index into the class's method table, the same lookup WrenLift's own send performs.
* Haxe keys a site by the object's `hl_type`. It stores a field's byte offset and type, or a method's slot in the type's method table. It reads the slot on every call, because the slot is where the tier installs a promoted body.

A site is shared. Several threads may reach it, and a stale read costs one extra lookup.

**Epochs:** Whatever a site holds was filled in a particular *epoch* (`protocol::epoch`), and it is only read back in that same epoch. A reload bumps the epoch, so every site in every language refills on its next use. This matters because a method a site found, a slot, or a direct send may refer to a body that no longer exists after a reload. The cost is one load and one compare per send, in addition to the key check.

**Direct sends:** A callee that can perform the entire send for a site in one function stores that function in the site as a *direct send*, together with two words of its own. The bridge calls the direct send before anything else, passing the receiver's address or the typed callable's function as the target. Only when the direct send returns `Missing` or `Unsupported` does the bridge forget it and take the plain path, which fills the site again. A caller with nothing to convert can request the direct send alone with `call_direct_at` and take the plain path itself when there is none. The site counts how many sends took the plain path, for the run report.

* Wren stores the closure it found and the class it found it on. The next call checks that the receiver is that class or an instance of exactly that class, then dispatches.
* Ash's typed dispatcher stores the signature's kinds, read once and kept per signature. The next call places the arguments by those kinds and calls the code, unless the cell holds a stub again.
* A Haxe field read or write stores the field's offset and type under the object's `hl_type`. The next send checks the type and reads or writes in place.
* A ref that forwards to a Wren object keeps no direct send, because the send would be applied to the ref rather than the object.

## Guards

Haxe leaves its code with a long jump, so a call into Haxe needs a frame below it where a throw can land. Arming a trap on every call is the per-crossing cost. A guard arms one trap per *run* instead.

* **Registration.** The language that performs long jumps registers the guard with `set_guard`. The guard is a function that runs a body under the language's trap and returns `Raised`, with the error pending, when a throw landed.
* **Use.** `run_guarded` runs a body under the guard. `guarded` reports whether the current thread is under one. Under a guard, a call into Haxe arms nothing: a throw lands at the guard, and none of the frames in between own anything that a drop would need to release.
* **Landing rule.** A throw must land below the run it came from and above the frames of the language that entered the run. A run may skip the guard when its entry site has never seen a call back into Haxe. In that case the guards the thread is under are set aside for the duration of the run, and the run's crossings back into Haxe catch throws for themselves. The first such crossing marks the entry site as reentrant (`CallSite::reentrant`), so the next run from that site is guarded. `enter` makes this decision for a site. A body that never calls back pays nothing, however deeply it is nested. A body that does call back pays one trap per entry instead of one per crossing.
* **Abandoned frames.** The frames a throw abandons belong to the other language. Whatever that language's adapter keeps per thread, it restores after `run_guarded` returns false. Anything the abandoned frames rooted on the stack is lost with them, which is why an adapter keeps an object it created for a call on its own frame rather than in a handle.

## Cells & Shadows

Word zero of a core object names its descriptor. Word zero of a HashLink object names a bare `hl_type`, a layout the core cannot add a prefix to. The two are distinguished by the `hl_type`'s mark bits: every descriptor names one static of the core's (`heap::CORE_MARK`), and no HashLink type does. `desc_of` returns the descriptor at word zero or, for a bare `hl_type`, the single foreign descriptor that the language with that layout registered through `set_foreign_descriptor`. A Haxe object is therefore already a core object and crosses as itself.

**Cells:** A language's compiled code reads its own objects at fixed offsets, so it can only hold an object from another language if that object has the holder's header. A *cell* (`caribou::cell`) is one core object that represents a foreign object to every language that holds it. Each holder finds its own header at the offset its code expects:

* Word zero is a descriptor that the holder's adapter fills so that the cell reads as the holder's own type. For Haxe this is an `hl_type` that mirrors a Haxe class, which makes the cell an instance of that class.
* Sixteen bytes in is a WrenLift instance header, so the cell plus 16 is an instance of an installed class to Wren.
* The cell's protocol forwards every message to the object it represents. This forwarding is what distinguishes a cell from any other core object. The cell's trace keeps the object alive; its drop hook forgets the object.
* A holder may place an object it constructed itself in front of a cell. The foreign object then always comes back as that constructed object.

See [haxe-imports.md](haxe-imports.md#cells) and [wren-imports.md](wren-imports.md#lifetime) for how each side uses cells.

**Shadows:** The same object crossing twice must produce the same cell, so the cell must be findable from the object. The protocol solves this with the *shadow*: the object's own language stores the cell on the object, where finding it is a single read. `shadow(lang)` returns the cell kept for a language. `keep_shadow` stores a cell when none is stored yet and returns the existing one otherwise. `drop_shadow` removes it when the cell dies. A Wren object stores its shadow in its bridge word (see [adapters.md](adapters.md#wren-objects)). Haxe objects have no spare word, so Haxe answers `Unsupported`, and the cell module tracks those cells in a map of its own.

## Diagnostics

`caribou::diag` prints an error in two steps. This is the same structure WrenLift and Zyntax already use.

* **`report(err)`** converts an `Error` value into a plain `Diagnostic`: the kind and message, one label per frame that has a span (its language, name, source id, and byte span), the frames without a span as notes in order, a note for a native payload, and the cause chain as nested diagnostics. The `Diagnostic` contains no ariadne types, so an adapter can pass it to its own renderer or give the core a diagnostic of its own.
* **`render(diag, sources, out, color)`** draws a diagnostic with ariadne: the kind and message as the header, each label on its source line colored by language from a fixed palette, the notes, and each cause as a further report whose message begins with `caused by:`. `sources` is a `SourceLookup`, a single method that maps a source id to its text. Tests implement it over a map; adapters implement it over their module tables. A label whose source cannot be found is rendered as a note. `render_string` returns the same output as a string.

Language names come from the process-wide table that `World::register` fills, available through `world::language_name`.
