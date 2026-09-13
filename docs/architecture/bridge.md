# Object protocol and bridge

This is the part of the core a value goes through when it leaves one
language for another. `caribou::protocol` is the message set,
`caribou::symbol` the names, `caribou::error` the error value,
`caribou::bridge` the call, and `caribou::diag` the report a driver
prints. Each adapter supplies its half: the protocol for its own objects,
and for Ash the typed dispatcher. How Haxe and Wren objects answer is in
[adapters.md](adapters.md).

## Values at the boundary

Two ABIs meet at every cross-language call. Typed HashLink code passes raw
scalars and pointers by signature. Every dynamically typed runtime passes
`caribou_abi::Value`, one NaN-boxed word. The core converts between them
once, at the edge, and never inside a language.

An object `Value` that reaches the bridge has a `TypeDesc` at word zero.
That is the protocol's contract. An adapter whose native objects carry a
bare `hl_type` wraps them before they cross.

## The protocol

Every heap object answers a closed set of messages through the `Protocol`
vtable its `TypeDesc` points at. An adapter implements the vtable once
for its own types. The core dispatches through it when a value crosses
into a language that did not make it.

| Message | Meaning |
|---|---|
| `get_member`, `set_member` | a named field or property, by interned symbol |
| `invoke` | call a named member with arguments |
| `get_member_at`, `set_member_at`, `invoke_at` | the same, through a call site the caller keeps (below); optional |
| `call` | call the object itself, if it is callable |
| `arity` | how many arguments `call` takes |
| `index`, `set_index`, `len`, `iterate` | sequence and map access |
| `to_string`, `hash`, `equals` | identity and display |
| `unwrap_native` | the native payload of a plugin object |
| `type_name` | what the registry publishes the object's class under |
| `is_error`, `error_message`, `error_kind`, `error_cause`, `error_trace` | the error protocol |

A message an object does not answer returns `Unsupported`, and the calling
language maps that to its own notion of a missing member. An entry that
raises sets the error pending on the current task and answers `Raised`.
Entries are `extern "C-unwind"`, so a panic inside a Rust entry travels to
the bridge's protected boundary instead of aborting inside the entry.

## Symbols

`caribou::symbol` is one interner per process. `intern` gives a `Symbol`
for a name and `name` gives the name back. The strings are leaked,
because a symbol lives as long as the process.

Interning takes a lock; reading does not. The entries live in chunks that
are allocated once and never move, indexed by id. An id is handed out
only after its entry is written and the table's length has been published
past it. So `name` and `hash` are two atomic loads and an index, which is
what a protocol entry on a hot call path can afford.

Each symbol also carries `hash`: HashLink's field hash of its name. It is
the loop Ash's `hlp_hash_gen` runs, over UTF-16 code units, `h = 223 * h
+ unit` in wrapping 32-bit arithmetic, then a truncating remainder by
`0x1FFFFF7B`. So a name hashes here to what `hashed_name` holds for it in
Ash. HashLink's own table also probes upward when two live names collide;
that depends on its cache and is not reproduced.

## Errors

`Error` is a heap object of the core's own language, `LANG_CORE`. It is a
`KIND_DYNAMIC | TRACED` allocation under a static `TypeDesc`. Its trace
hook marks its fields, and its protocol answers the five error messages,
`to_string`, and `get_member` for `kind`, `message`, `cause`, `native`,
`origin` and `trace`.

Its fields are:

- the kind, from `caribou_abi::ErrorKind`;
- the language that raised it;
- a message, a core `Str`: UTF-8 bytes after a two-word header, traced
  with no children, so the bytes are never scanned;
- an optional cause;
- an optional native payload, the originating language's own error
  object, kept so that a round trip unwraps to it;
- a trace, a core `Trace`: a fixed-capacity list of frames, replaced by a
  larger copy when it fills.

A frame records the language crossed, the callable's name, and, when
known, the source it was in (a file path or module name, as a core `Str`)
and a byte span into that source. `push_frame` records a frame with a
source and a span. `push_segment` records one with neither, which is what
the bridge does at a boundary.

`with_native` builds a `User` error around a native payload. `from_value`
tells an `Error` from any other value by its descriptor.

Nothing in this module is boxed; every object is reached through a raw
pointer or a `Value`. A NaN-boxed `Value` on the stack is invisible to the
conservative scanner. So the module roots every object it creates by a
handle, from allocation until it is stored in a rooted parent or returned,
and roots every object it holds across an allocation.

## The pending error

An error leaves a language as a pending value, not as an unwinding
exception. `bridge::set_pending` stores it for the current task,
`take_pending` returns it and clears the slot, and `has_pending` asks.

The slot is a thread-local map keyed by task id, rather than a
`HostState`. A task has one host-state slot and it belongs to the adapter
that spawned it, and a task never leaves the world that created it, so
this thread's map holds exactly one slot per task of this world. The slot
roots its value through a handle while it waits, since the map is not a
heap root. A task that finishes without its error being taken leaves an
entry behind; the next insert past a small threshold sweeps such entries
away.

## Typed dispatch

A typed callable is a C function pointer with an `hl_type_fun`-shaped
signature and the language it belongs to. `Callable::Typed` holds the
pointer. `Callable::Cell` holds the address of a cell the pointer is read
from at each call. That is how Ash publishes a function, because its
`functions_ptrs` entry is where the tier installs a promoted body.

The bridge does not marshal such a call itself. Each language registers a
`TypedDispatch` with `set_typed_dispatch`: a C-ABI function that takes the
function, the signature, the arguments as `Value`s and an out slot, and
answers a reply code. The dispatcher is read from a table of atomics by
language id.

Ash's dispatcher calls compiled code directly, under its own id. When the
pointer is real code rather than one of the interpreter's stub sentinels,
and the arity matches, the arguments are placed by the signature's kinds
through `ash_native_call`, with no boxing, under a HashLink trap. A stub,
a mismatch, or a kind the direct path does not take goes through
`hlp_dyn_call`, as a dynamic call in Ash does. The other direction, a
call from Haxe into the bridge, is a host native called by record; see
[haxe-imports.md](haxe-imports.md#binding-the-natives).

The core registers a default for `LANG_CORE` that covers what the core's
own callables and the tests need: up to four arguments of kinds `HI32`,
`HBOOL`, `HF64` and `HDYN`, returning `HVOID`, `HI32`, `HBOOL`, `HF64` or
`HDYN`, by transmuting the function to the C type those classes describe.
It relies on integer-class arguments of any width sharing a register or a
slot, which holds on the native ABIs the core runs on and not on wasm.

## Calls

`bridge::call(callable, args, caller)` invokes a callable on behalf of the
language `caller`.

- A dynamic callable is sent `call`.
- A typed one is checked for arity against its signature and handed to
  its language's dispatcher.
- A Wren method (`Callable::WrenMethod`: a class, a Wren signature such
  as `hit(_)`, and whether it is static) is an `invoke` of that signature
  on the first argument, or on the class for a static or a constructor.
  So the call lands in the Wren protocol like any other message.

`call_named` does the same with the callee's name for the trace. `invoke`,
`get` and `set` send `invoke`, `get_member` and `set_member` with the same
handling.

Every crossing guarantees three things.

It is protected. The dispatch runs under `catch_unwind`, and a panic
becomes an `Internal` error carrying the panic's message. An entry that
answered `Unsupported` or `Missing` becomes a `Type` or `Runtime` error
naming the value and the member. An entry that answered `Raised` yields
the pending error; a pending value that is not an `Error` is wrapped in
one as its native payload, so it still unwraps at home.

Every error leaving a call carries one more trace frame: the callee's
language, its name if the caller gave one and `<callable>` otherwise, and
no source.

A value returning to the language that raised it is unwrapped. When the
error's native payload is set and its origin is `caller`, the call returns
the payload rather than the `Error`. So a Haxe exception that passed
through Wren and back is the same Haxe object it was.

## Call sites

A caller that makes the same send from one place keeps a `CallSite` for
it, and uses `invoke_at`, `get_at`, `set_at` or `call_at`. A site is three
words. The callee's protocol fills them with what it derived for the site,
under a key of its own choosing, and reads them back when the key matches.
A site that sees the same shape again does no lookup.

The protocol entries `invoke_at`, `get_member_at` and `set_member_at` are
optional; the bridge falls back to the plain ones.

Wren keys a site by its VM and keeps the interned signature symbols in it.
After that, finding the method is an index into the class's method table,
as wren_lift's own send does. Haxe keys a site by the object's `hl_type`.
It keeps a field's byte offset and type, or a method's slot in the type's
method table, and reads the slot per call, because the slot is where the
tier installs a promoted body.

A site is shared. It may be reached from several threads, and a stale
read costs one lookup.

## Diagnostics

`caribou::diag` prints an error in two steps, the shape wren_lift and
zyntax already use.

`report(err)` turns an `Error` value into a plain `Diagnostic`: the kind
and message, one label per frame that has a span (its language, name,
source id and byte span), the frames without a span as notes in order, a
note for a native payload, and the cause chain as nested diagnostics. No
ariadne type appears in it, so an adapter can feed it to its own renderer,
or hand the core a diagnostic of its own.

`render(diag, sources, out, color)` draws one with ariadne: the kind and
message as the header, each label on its source line coloured by language
from a fixed palette, the notes, then each cause as a further report whose
message begins `caused by:`. `sources` is a `SourceLookup`, one method from
a source id to its text, implemented over a map in tests and over module
tables in adapters. A label whose source cannot be found is written as a
note. `render_string` returns the same as a string. Language names come
from the process-wide table `World::register` fills,
`world::language_name`.
