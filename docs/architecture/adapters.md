# Adapters: how each runtime answers the protocol

Each adapter's `Runtime` implements `world::Adapter` with one language,
`haxe` or `wren`. `World::register` hands it an id. The adapter writes the
id into the descriptor its objects carry, and Ash also registers its typed
dispatcher under it.

The order at startup is fixed. Both seams first (`caribou_ash::install`,
`caribou_wren::install`), before either runtime allocates. Then the world
and its registrations. Then the first VM. Registration comes before the VM
because the id it writes is read from the descriptor by every message and
every collection from then on.

## Haxe objects

A Haxe object crosses wrapped. Its word zero is a bare `hl_type`, which
has no protocol slot, so `caribou_ash::wrap` makes a `HaxeRef`: a two-word
core object under a static descriptor. Its trace hook marks the object.
Its protocol reaches the object the way compiled Haxe does.

- `get_member` and `set_member` find a declared field in the runtime's
  own lookup tables, `hl_runtime_obj` up the class chain. The entry there
  holds the field's byte offset and type, and the value is read or written
  in place by kind, with nothing boxed. A name that is not a declared field
  goes through `hlp_dyn_getp` and `hlp_dyn_setp`, as a dynamic access
  does. The field hash is `hlp_hash_gen` of the symbol's name, computed
  once per symbol.
- `invoke` finds the method's slot in the type's own method table, so an
  override wins and a promoted body is what the slot holds. It calls the
  method through the typed dispatcher with the object as `this`. A name
  that is a field holding a closure calls that closure.
- `call` runs a closure. A closure whose function is one of the
  interpreter's stubs is resolved to its compiled code through the module
  context, when it has any, and called directly with its bound value
  first. Otherwise it goes through `hlp_dyn_call`.
- `to_string` is `hlp_value_to_string`. `type_name` is the class's name,
  which is what the registry publishes it under. `equals` and `hash` are
  the wrapped object's identity, so two wrappers of one object compare
  equal, and no cache is kept. `unwrap` gives the object back.

A Haxe `String` is not wrapped. It crosses as a core `Str`, and a core
`Str` entering Haxe becomes a fresh `String` under the type the loaded
program's `String` class carries. Sequence access is not answered yet.

### Calling into Haxe

Ash's dispatcher is described under [typed dispatch](bridge.md#typed-dispatch).
When it cannot call directly, it builds a `vclosure` with no bound value
around the code pointer and its signature, boxes each argument by the
signature's kind into the `vdynamic` `hlp_dyn_call` takes (an object
argument is the wrapped object itself), and unboxes the result by the
return kind.

Every call into Haxe code runs under a HashLink trap. The trap's setjmp
frame is a C function of the adapter's own (`trap.c`), and its context
lives in that frame too: `hlp_setup_trap_in` arms it there, so the
runtime allocates and pools nothing for it, and `hlp_remove_trap_in` pops
it by its storage after a normal return. An `hl_throw` inside lands in
that frame instead of unwinding through Rust. The thrown value becomes a
core `Error`. A bytes
value is the runtime's own error, and its kind is read from the message
(`Null access`, out of bounds, divide by zero); a String or any other
object is a `User` error. The exception itself is the error's native
payload, wrapped, so it is the same object when it returns to Haxe.

## Wren objects

A Wren object crosses as its own core address: the start of the prefixed
allocation, sixteen bytes before the address wren_lift holds. Word zero
is the descriptor its VM's heap record carries, which is how a message
finds the VM the object belongs to. Word one is the bridge word: the
object another language keeps standing for this one (the protocol's
shadow, below), and three flag bits of the adapter's own. `caribou_wren::wrap`
and `unwrap` translate, and every protocol entry adds the prefix back
before touching the object.

Strings cross by value in both directions. A Wren string leaving becomes
a core `Str` at the edge, whichever entry or conversion it leaves through,
and a core `Str` entering becomes a Wren string. So a Wren string reaching
Haxe is a `Str`, which Ash turns into a Haxe `String` as it does any
other. A core int becomes a Wren number on the way in, Wren having no
other.

An object of another language entering Wren is one of two things. If it
stands for one of this VM's own objects (a `WrenRef`, see
[haxe-imports.md](haxe-imports.md), answers `unwrap_native` with the
object it holds), it becomes that object again, so identity survives a
round trip. Otherwise it becomes an instance of the class installed for
its type, when its language has published one (see
[registry.md](registry.md)). It cannot enter Wren any other way.

The protocol answers through the runtime's own methods, by Wren's
signature convention:

| Message | Wren method |
|---|---|
| `get_member` | the getter `name`, else an instance field of that name |
| `set_member` | `name=(_)`, else the field |
| `invoke` | `name(_,_)` by arity, or the full signature when given one; the getter when the arity is zero and there is no `name()` |
| `call` | the closure itself |
| `index`, `set_index` | `[_]`, `[_]=(_)` |
| `len` | `count` |
| `iterate` | `iterate(_)` and `iteratorValue(_)` |
| `type_name` | the name the class was published under (`hud.Hud`), else its bare name |
| `shadow`, `keep_shadow`, `drop_shadow` | the bridge word: one object of one language, whichever keeps one first |

A method the class lacks raises Wren's own `does not implement` error.
wren_lift's error is a message string, so a Wren error crossing the bridge
is a core `Error` whose native payload is that message as a core string.
No Wren object is an error by itself.

### Dispatch

The protocol answers a message the way wren_lift's compiled code answers
a send. The signature's symbols, `hit(_)` and its `static:` twin, are
interned once per VM. They are kept on the heap record by core symbol and
shape, and in the caller's call site when it has one. After that the
method is an index into the receiver's class's method table, or into the
class's own table when the receiver is a class.

What it finds goes to `dispatch_method_pub`, the dispatch wren_lift's own
`wren_call_N` runtime entries use once they have found a method. A
compiled body is called with its context set. A trivial getter or setter
reads or writes the field directly. A native or a constructor takes its
own path. And the tier is ticked for a body that is not compiled yet, so
a method only ever called from another language still compiles. A
constructor is ticked here for the same reason. A `Fn` called through
`call` goes through `call_closure_jit_or_sync` with no receiver, and is
ticked the same way.

Arguments cross into a buffer on the stack, with a slot for the receiver
before them. They go on the heap only past what a compiled body takes in
registers.

A message with a call site leaves a direct send in it once it has found
a closure or a constructor (see [call sites](bridge.md#call-sites)): the
closure and the class, under the VM's record. The direct send checks
that the object is this thread's VM's and that the receiver is that
class or an instance of exactly it, then dispatches as above; otherwise
it answers `Missing` and the plain path decides.

### Fibers

A Wren fiber runs on a krio stack of its own in every VM the driver or
the runner makes (`krio_fiber_active`). wren_lift tells its seam when a
stack is made, suspended with its stack pointer, and freed; the adapter
registers each with the core's heap under krio's id, so a collection
scans a suspended fiber from where it stopped, as it scans the core's
own tasks.

### The VM an entry runs on

The Wren entries run on a VM, and `install` cannot know which. Whoever
creates a VM enters it: `enter_vm` and `leave_vm` around a run, or
`with_vm` around a call that may reach Wren objects. The runner enters its
VM for the whole run. An entry with no VM entered falls back to the one
wren_lift reports as dispatching, and raises an `Internal` error if there
is none.

Every entry that takes a receiver first checks that the object belongs to
the entered VM, by the record address in its prefix, and raises otherwise.
A value of another VM, or of one that is gone, must not run on this one.

## Functions across the bridge

The protocol's `arity` message marks a callable. A Wren closure answers
its function's arity, a Haxe closure its visible type's, and a ref
forwards it.

A Wren function crossing into Haxe (`caribou-ash`'s `callback.rs`)
becomes a closure Haxe calls as its own. Where the native it crosses
through declares the function's type, it is Ash's *record closure*
(`hlp_alloc_record_closure`): a closure of that very type over a
`Callback` object holding the function's ref and the signature's kinds.
The ref, since a Wren object another language holds is held through
its shadow, which is what a Wren cycle looks for. Ash's
one entry for every signature places the argument registers as one word
each and calls the callback's entry, which reads them by kind, sends them
through the bridge, and answers the result as one word; nothing is boxed.
Where the type is not known, or is one the registers cannot take, it is
the var-args closure `Reflect.makeVarArgs` makes, `hlp_make_var_args`
over an inner closure bound to the function's ref, and ash packs a call
into the inner closure's array. Either entry throws a bridge error into
Haxe and answers null for a result Haxe has no form for. A closure made
either way going back is recognised by its entry and unwrapped to the
function.

A Haxe function crossing into Wren becomes an instance of `Function`, a
class the adapter installs on first need in the bridge's own module. Its
`call` natives, one per arity, and its `arity` reach the object behind the
instance through `bridge::call`. It is chosen over the published class's
proxy when the value answers `arity`.
