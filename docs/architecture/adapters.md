# Runtime Adapters & Protocol Implementation

## Overview

Each adapter implements `world::Adapter` for one language, `haxe` or `wren`. When the adapter registers, `World::register` assigns it a language id. The adapter writes that id into the descriptor of every object it creates. Ash also registers its typed dispatcher under the same id.

**Startup Ordering:** Startup follows a fixed sequence:

1. Both seams are installed (`caribou_ash::install`, `caribou_wren::install`) before either runtime allocates any memory.
2. The world is created and the adapters register with it.
3. The first VM is created.

Registration has to happen before the VM exists because, from then on, every protocol message and every collection reads the language id from the object descriptor.

## Haxe Objects

A Haxe object crosses the bridge as itself; nothing wraps it. Word zero of the object holds a bare `hl_type`, and the core tells a bare `hl_type` from a descriptor by its mark bits. Every object with a bare `hl_type` at word zero is handled by the one foreign descriptor that Ash registers at install time (`protocol::set_foreign_descriptor`). That descriptor belongs to the Haxe language and carries the protocol implementation described below, which reads the object the same way compiled Haxe code does.

* **`get_member` / `set_member`:** The adapter looks up the field in the runtime's own tables (`hl_runtime_obj`), walking up the class chain. Each table entry holds the field's byte offset and type, so the adapter reads or writes the value in place, by kind, without boxing. A name that is not a declared field falls back to `hlp_dyn_getp` and `hlp_dyn_setp`, the same path a dynamic access takes. The field hash is `hlp_hash_gen` applied to the symbol's name, computed once per symbol.
* **`invoke`:** The adapter looks up the method's slot in the type's own method table. Because the lookup uses the object's own type, an override wins, and the slot holds whatever body the tier has promoted. The adapter then calls the method through the typed dispatcher with the object as `this`. If the name is a field that holds a closure, the adapter calls that closure instead.
* **`call`:** The adapter runs a closure. If the closure's function is one of the interpreter's stub sentinels, the adapter resolves it to compiled code through the module context (when compiled code exists) and calls it directly with the bound value first. Otherwise the call goes through `hlp_dyn_call`.
* **`to_string`** delegates to `hlp_value_to_string`. **`type_name`** returns the class name, which is the name the registry publishes the class under. **`equals`** and **`hash`** use object identity. **`unwrap`** returns the object itself.

**Strings:** A Haxe `String` is never wrapped. It crosses as a core `Str`. A core `Str` entering Haxe becomes a new `String`, allocated with the `String` type of the loaded program.

**Arrays:** A HashLink array is an object that declares a `length` field and `getDyn`/`setDyn` methods. Only these objects answer `len`, `index`, `set_index`, and `iterate`. The adapter caches the array layout once per type: the offset of the length field and where the elements are stored. For an `hl.types.ArrayBytes_*` the elements are behind the `bytes` field, one value of the class's kind each. For an `hl.types.ArrayObj` they are behind the `array` field, as a native array of pointers. The adapter reads and writes elements within the current length directly, without boxing. Any other array kind, and any write past the current length (which grows the array), goes through `getDyn` and `setDyn` as direct calls.

### Calling into Haxe

Ash's dispatcher is described under [typed dispatch](bridge.md#typed-dispatch). When the dispatcher cannot call compiled code directly, it takes the dynamic path: it builds a `vclosure` with no bound value around the code pointer and its signature, boxes each argument by the signature's kind into the `vdynamic` that `hlp_dyn_call` expects (an object argument is passed as the wrapped object itself), and unboxes the result by the return kind.

**Trap Frames:** Every call into Haxe code runs under a HashLink trap, unless the thread is already under the bridge's guard (see below). The trap's `setjmp` frame lives in a C function that belongs to the adapter (`trap.c`), and the trap context lives in that frame too. `hlp_setup_trap_in` arms the trap there, so the runtime does not allocate or pool anything for it, and `hlp_remove_trap_in` removes it by its storage address after a normal return. An `hl_throw` inside the call lands in that frame instead of unwinding through Rust.

**Error Conversion:** The adapter converts a thrown value into a core `Error`:

* A bytes value is one of the runtime's own errors. The adapter reads its kind from the message text (`Null access`, out of bounds, divide by zero).
* A `String` or any other object becomes a `User` error. Its message is the string itself, the message of a `haxe.Exception` (read from the field where the exception stores it), or otherwise the class name. No user code runs while the error is built.
* The original exception becomes the error's native payload, so it is the same object when it returns to Haxe.

**Guard Supply:** Haxe leaves its code with a long jump, so Ash provides the bridge's guard (`bridge::set_guard`). The guard is the same trap, armed once around a whole run of another language's code. A throw from any Haxe call inside that run lands at the guard. Under a guard, a call into Haxe does not arm a trap of its own; it only marks the run's entry site as one that calls back (see [guards](bridge.md#guards)).

## Wren Objects

A Wren object crosses the bridge as its core address: the start of the prefixed allocation, sixteen bytes before the address WrenLift itself holds. Word zero is the descriptor of the VM's heap record, which is how a protocol message finds the VM that owns the object. Word one is the bridge word. It holds the object from another language that represents this one (the protocol's shadow, described below) plus three flag bits the adapter uses. `caribou_wren::wrap` and `unwrap` convert between the two addresses, and every protocol entry adds the prefix back before touching the object.

**Value Conversion:**

* Strings cross by value in both directions. A Wren string leaving the VM becomes a core `Str` at the boundary, whichever entry or conversion it passes through, and a core `Str` entering the VM becomes a Wren string. A Wren string that reaches Haxe arrives as a `Str`, which Ash converts into a Haxe `String` as usual.
* A core int becomes a Wren number on entry, because Wren has a single numeric type.

**Foreign Objects Entering Wren:** An object from another language enters Wren in one of two ways. If it is a cell that holds one of this VM's own objects (see [haxe-imports.md](haxe-imports.md#cells)), it becomes that object again, so identity survives a round trip. Otherwise Wren holds it as an instance of the class installed for its type, as long as its language has published one (see [registry.md](registry.md)). That instance is either the view a cell keeps for Wren or, for any other object, an instance allocated on the heap (see [wren-imports.md](wren-imports.md#lifetime)). There is no other way in.

**Protocol Mapping:** The protocol maps each message onto the runtime's own methods, following Wren's signature convention:

| Message | Wren Method |
|---|---|
| `get_member` | The getter `name`, otherwise an instance field of that name |
| `set_member` | `name=(_)`, otherwise the field |
| `invoke` | `name(_,_)` selected by arity, or the full signature when one is given; the getter when the arity is zero and no `name()` exists |
| `call` | The closure itself |
| `index`, `set_index` | `[_]`, `[_]=(_)` |
| `len` | `count` |
| `iterate` | `iterate(_)` and `iteratorValue(_)` |
| `type_name` | The name the class was published under (`hud.Hud`), otherwise its bare name |
| `shadow`, `keep_shadow`, `drop_shadow` | The bridge word, which holds one object of one language, whichever claims it first |

A method the class does not define raises Wren's own `does not implement` error. WrenLift represents an error as a message string, so a Wren error crossing the bridge becomes a core `Error` whose native payload is that message as a core string. No Wren object is an error in itself.

### Dispatch

The protocol answers a message the same way WrenLift's compiled code performs a send.

* **Signature Interning:** The adapter interns the signature symbols (`hit(_)` and its `static:` counterpart) once per VM. It stores them on the heap record, keyed by core symbol and shape, and in the caller's call site when there is one. After that, finding the method is an index into the receiver's class method table, or into the class's own table when the receiver is a class.
* **Method Dispatch:** The resolved method goes to `dispatch_method_pub`, the same dispatch that WrenLift's own `wren_call_N` runtime entries use after they have located a method. A compiled body runs with its context set. A trivial getter or setter reads or writes the field directly. A native or a constructor takes its own path. The adapter ticks the tier for any body that is not compiled yet, so a method that is only ever called from another language still gets compiled; constructors are ticked for the same reason. A `Fn` invoked through `call` goes through `call_closure_jit_or_sync` with no receiver and is ticked the same way.
* **Argument Passing:** Arguments are copied into a stack buffer with one slot reserved for the receiver in front of them. Only arguments beyond the number a compiled body takes in registers go on the heap.
* **Direct Sends:** Once a message with a call site has found a closure or a constructor, it stores a direct send in the site (see [call sites](bridge.md#call-sites)): the closure and the class, keyed by the VM's record. The direct send checks that the object belongs to this thread's VM and that the receiver is that class or an instance of exactly that class, then dispatches as above. Otherwise it returns `Missing` and the plain path takes over.

### Fibers & Runs

In every VM the driver or the runner creates, a Wren fiber runs on its own krio stack (`krio_fiber_active`). WrenLift reports to its seam when a stack is created, when it is suspended (along with the stack pointer), and when it is freed. The adapter registers each stack with the core heap under krio's id. A collection scans a suspended fiber from the point where it stopped, the same way it scans the core's own tasks.

**Guarded Runs:** Every run of a fiber, including WrenLift's own, goes through the seam's `run_guarded`, which the adapter fills with the bridge's guard. A Haxe throw inside the run lands at the base of the fiber's run, on the fiber's own stack, and is raised on the VM as a runtime error; `Fiber.try` sees it the same way it sees an abort. The adapter's own entries into compiled code (a direct send, an `invoke`, a `call`, a constructor) go through `bridge::enter`, which applies the guard when the entry site asks for it. When a throw lands, the adapter restores the thread's JIT state and the interpreter's live register files on the stack to their values at entry (`JitMark`), because the frames in between are gone.

**Stack Switches:** WrenLift reports to the seam every stack switch it performs itself: a `Fiber.call` into a fiber with its own stack, and the return from it (`stack_switch`). The adapter forwards each switch to the core, so the per-stack state that the adapters keep moves with the switch (see [scheduler.md](scheduler.md#the-scheduler-loop)).

### The World

WrenLift's scheduler has the same shape as the core's: worlds, tasks, wait tokens, and a pool. Under the adapter there is exactly one world, the core's. The World slots of the seam (`world.rs`) route WrenLift's waits and tasks into it:

* A token is a core token, bound to whichever context parks on it.
* `Fiber.spawn` creates a task on the calling world. `Thread.create` creates a task on the least-loaded worker world.
* `Fiber.tick`, `Fiber.idle`, and the main program's park drive the core's scheduler turns.

As a result, a Wren fiber waiting on a lock that a Haxe thread releases lets every other task run in the meantime, and the same is true the other way around (see [scheduler.md](scheduler.md#guest-runtime-tasks)).

**Wren Tasks:** A Wren task is a context that WrenLift creates. It holds the program, the closure until the fiber exists, and the fiber after that. The core steps the task through WrenLift's `task_step`, which runs the fiber until its next park or yield, on the world the task was placed on. The fiber is created on that world, because a fiber has to run on the thread that created it, and a worker in the core's pool gets a view of the program the first time it steps one of these tasks. The task's `Suspend` is WrenLift's own switch (`task_suspend`), which keeps the roots of compiled frames alive across the switch. A Haxe call made from a Wren task parks through the same switch.

**View State:** WrenLift's collector requires every thread that holds a view to be either safe or polling. A thread in the core's world is neither on its own: it runs tasks from any language and idles inside the core. So the view follows the core's own state transitions, and the seam's thread slots never hear about them, because the thread is never in a WrenLift wait. The view is running while the thread runs, safe inside the core's blocking regions (through the blocking hook), and safe at a core safepoint while WrenLift's world is requesting a stop (through the safepoint hook; the `host_poll` slot tells the core about the request and bumps the poll epoch so compiled loops reach a safepoint).

Each context carries the view's state across the core's switches as a `ViewState` host state, attached to every task and to the main context. A context parked inside Wren leaves the view safe; a context resuming into Wren finds it running. Each context also carries the run it is in (`WrenActivation`): the fiber, its error state, and the JIT's per-thread state form one set per view. A context sets its run aside when the thread switches away (`VM::set_aside`) and picks it up again on the way back; in between, the run's roots belong to the view. `task_step` marks a worker's view as running for the duration of the step, and a view that the host's world created on a worker stays with the program's last task on that worker. A wait for a WrenLift collection is reported to the seam, because the core's collector may need the thread while it waits.

Inside a core fiber, WrenLift's collector scans nothing on the thread. It scans only the stacks it can find: its own fibers, and the thread's own stack when the call chain ends there. Objects held only by a stack it cannot find are kept alive by the core's collection, like everything else that WrenLift's marking did not reach (see [heap.md](heap.md#hosted-collectors)).

### Threads & Isolates

WrenLift's `Thread` runs tasks over the program's heap, on the core's pool under the adapter. `Isolate` runs a whole VM on a dedicated thread.

* **Mutator Registration:** A thread reports to the seam when it starts running Wren code and when it stops. The adapter registers each such thread as a core mutator for that span, the same as it does for the thread that created the heap.
* **Blocking Regions:** WrenLift's world knows when one of its threads is safe (in a wait or in a native call) and reports that to the seam along with the stack pointer and the register range it published. The adapter maps that state onto the core's blocking region. The core's collector can then scan the thread where it stands without waiting for it, and it holds the thread if it resumes while a collection is in progress (see [heap.md](heap.md#collection-cycle)).
* **Stop Hook:** The core's stop hook works in the other direction. The adapter answers it with WrenLift's own stop mechanism, which makes the poll pages unreadable until every running thread has faulted into the same safe transition.
* **Sharded Pins:** A heap record's pins, and the cells that Wren holds, are stored per thread in a shard that only the owning thread appends to. A collection cycle and the anchor's trace read every shard while all other threads are stopped, so no allocation needs a lock for them.

### The VM an Entry Runs On

The Wren protocol entries run on a VM, and `install` cannot know which one. Whoever creates a VM has to enter it: `enter_vm` and `leave_vm` around a run, or `with_vm` around a call that may reach Wren objects. The runner enters its VM for the whole run. An entry with no entered VM falls back to the VM that WrenLift reports as currently dispatching, and raises an `Internal` error if there is none.

Every entry that takes a receiver first checks, using the record address in the object's prefix, that the object belongs to the entered VM, and raises if it does not. A value from another VM, or from a VM that has been destroyed, must never run on this one.

## Functions Across the Bridge

The protocol's `arity` message identifies a callable. A Wren closure answers with its function's arity, a Haxe closure with the arity of its visible type, and a ref forwards the message.

**Wren Functions Entering Haxe:** A Wren function that crosses into Haxe (`caribou-ash`'s `callback.rs`) becomes a closure that Haxe calls as one of its own.

* When the native the function crosses through declares the function's type, the adapter creates Ash's *record closure* (`hlp_alloc_record_closure`): a closure of that type over a `Callback` object that holds the function's ref and the signature's kinds. The adapter holds a ref rather than the object itself because a Wren object held by another language is held through its shadow, which is what a Wren collection cycle looks for. Ash provides one entry for every signature. That entry stores the argument registers as one word each and calls the callback's entry, which reads each word by kind, sends the arguments through the bridge, and returns the result as one word. Nothing is boxed.
* When the type is unknown, or is one the registers cannot carry, the adapter creates the variadic closure that `Reflect.makeVarArgs` produces: `hlp_make_var_args` over an inner closure bound to the function's ref. Ash packs each call into the inner closure's argument array.
* Either entry throws a bridge error into Haxe when the call fails, and returns null for a result that Haxe has no representation for. A closure created by either path is recognized by its entry when it crosses back and is unwrapped to the original function.

**Haxe Functions Entering Wren:** A Haxe function that crosses into Wren becomes an instance of `Function`, a class the adapter installs on first use in the bridge's own module. Its `call` natives (one per arity) and its `arity` reach the underlying object through `bridge::call`. The adapter picks this class over the published class's proxy whenever the value answers `arity`.

**Sequences Entering Wren:** An object from another language that answers `len` but has no published class, such as a Haxe array, becomes an instance of `Sequence`. The adapter installs this class the same way and makes it extend Wren's own `Sequence`. Its `[_]`, `[_]=(_)`, and `count` map to the bridge's `index`, `set_index`, and `len`, and its `iterate` and `iteratorValue` walk the sequence by position, so `for`, `toList`, `map`, and the rest of Wren's sequence methods work on it. When it crosses back, it is the original array.
