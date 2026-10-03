# Native Plugins & The Shared ABI

## Overview

A plugin is native code that every hosted language reaches the same way: a dynamic library built on the shared ABI (`caribou_abi`). The library exports a table that names its functions, the class each function belongs to, and their signatures. A language sees exactly what the driver loaded and nothing more. No language reaches a plugin through a path of its own.

**Language-native libraries:** Each language's own native libraries keep working for that language, without conversion:

* A Haxe program's `hdll` libraries next to it, loaded by Ash.
* A Wren module's hatch packages and their plugins, loaded by WrenLift (see below).

What one language gets this way, another language can reach through the bridge by importing the class that wraps it. A Caribou plugin is what every language reaches directly.

## Writing a Plugin

A plugin crate depends on `caribou_abi` and nothing else from Caribou, and builds as a `cdylib`. Its functions are ordinary Rust items: `extern "C"` functions over the types the ABI tags cover. A class is a type whose associated functions belong to it. The crate reads, and the tooling treats it, like any other Rust module. One `plugin!` invocation then declares what is exported, by signature, the way a header file would:

```rust
pub extern "C" fn hypot(a: f64, b: f64) -> f64 { a.hypot(b) }
pub extern "C" fn same(v: Value) -> Value { v }

pub struct Vec2 { x: f64, y: f64 }
impl Vec2 {
    pub extern "C" fn new(x: f64, y: f64) -> Box<Vec2> { Box::new(Vec2 { x, y }) }
    pub extern "C" fn len(this: &Vec2) -> f64 { this.x.hypot(this.y) }
    pub extern "C" fn scale(this: &mut Vec2, k: f64) { this.x *= k; this.y *= k; }
    pub extern "C" fn dot(this: &Vec2, other: &Vec2) -> f64 { this.x * other.x + this.y * other.y }
}

caribou_abi::plugin! {
    name: "math";
    fn hypot(f64, f64) -> f64;
    fn same(Value) -> Value;
    class Vec2 {
        fn new(f64, f64) -> Box<Vec2>;
        fn len(&Vec2) -> f64;
        fn scale(&mut Vec2, f64);
        fn dot(&Vec2, &Vec2) -> f64;
    }
}
```

**Declaration rules:**

* A function outside any class becomes a static method of a class named after the plugin (`Math`). This is the plugin ABI's own convention; a Zyntax module's functions stay the module's own (see [zyntax.md](zyntax.md)).
* Each declaration is checked against the item it names by coercing the item to the declared function pointer type. A signature that does not match the item does not compile.
* The macro reads each type's tag from the `Param` and `Returned` traits. `u8`, `u16`, `i32`, `i64`, `f32`, `f64`, `bool`, `()`, and `Value` map to their tags. `Text` is a string. `&T` and `&mut T` of a declared class are an object of that class, borrowed for the call; when such a parameter comes first, the function is an instance method. `Box<T>` is a new object of the class, owned by the core from then on.
* A static `new` that returns its own class is the class's constructor.

**Generated code:** The macro writes the `SymbolDesc` table, the class table with one finalizer per class (which drops the `Box`), the `PluginInfo`, and the two symbols every plugin exports: `caribou_abi_version`, which a core compares with its own version before binding anything, and `caribou_plugin_entry`, which receives the core's table (see below) and returns the plugin's.

## Loading

`caribou_plugin::load` opens a library, rejects it if it was built against a different ABI version, and reads its table. `load_dir` opens every library in a directory that exports the entry point and skips the ones that do not. The driver loads the plugins a project file declares (`[plugins]`) before the program starts; for a `.hl` opened directly, the ones in `plugins/` next to it. A bundle carries plugins as native library sections (see [bundle.md](bundle.md)), and a session started from a bundle loads those.

## Plugins as Languages

The plugin adapter (`caribou_plugin::Runtime`) registers one language per plugin, named after the plugin. Every plugin therefore has a namespace without any configuration:

* **Wren:** `import "math:Math" for Math`, the same way it imports any other language's class.
* **Haxe:** `import math.Math`. The build macro describes the libraries in `plugins/` next to the compiler output (`caribou describe` reads a plugin the same way it reads a Wren module, producing one module per class) and emits a class for each, using the plugin's name as the package. The emitted class gets the same natives a Wren class gets. The driver's namespaces cover the plugins next to the program, so the natives bind by name at run time. An instance method of a plugin class is a typed target that the Haxe adapter calls with the object as the first argument, whereas a Wren object's member is sent to the object for Wren's own dispatch. The faces the macro emits are not published as Haxe classes, because they belong to another language.

**Publishing and dispatch:** The table is published to the registry as one module per class, named after the class, with the class's functions as its methods. Every target is a `Callable::Typed` whose signature is an `hl_type` of kind `HFUN` built from the tags. There is one signature object per distinct signature in the process. The plugin's language has a typed dispatcher: it reads the signature's kinds, passes each `Value` as the word for its kind (an integer as itself, a float as its bits, a bool as 0 or 1, a `DYN` as the value's bits), calls the symbol, and reads the result back the same way. Nothing is boxed. A value that a kind cannot represent produces a `Type` error that names the argument.

**Word thunks:** The target's code is not the function itself. For each symbol, `plugin!` writes a thunk of one shape, `unsafe extern "C" fn(*const u64) -> u64`: it reads each argument from its word as the function's own parameter type, calls the function, and returns the result as a word. A word holds a value in its low bytes. The dispatcher therefore makes one kind of call for every signature, and needs no table of signatures. That matters on wasm, which checks every indirect call's signature: a program there could only carry such a table as one entry per possible signature, and links only the thunks of the plugins it has.

## Objects

An instance of a plugin class is a core object with the class's descriptor. It is two words: the descriptor and the payload that the constructor's `Box` produced.

* **Identity:** The descriptor, one per class per process, is the class's identity. It carries the class's type name (`math.Vec2`), so a language installs its class for the object the same way it does for any published type. In Wren, `Vec2.new(3, 4)` produces an instance that adopts the object, and `v is Vec2` is true.
* **Lifetime:** The descriptor's drop hook runs the plugin's finalizer when the object dies, during the sweep. The payload therefore lives exactly as long as something holds the object.
* **Protocol:** The object answers its type name, uses identity for `equals` and `hash`, and returns the payload from `unwrap_native`. The plugin's own functions are what operate on it, through the classes the languages installed.

**Typed object parameters:** In a signature, an object parameter or result is typed by the descriptor itself, which is an `hl_type` at word zero. When the dispatcher sees a descriptor where a scalar kind would be, it takes the argument's object (through the cell another language holds it by), checks that the object's descriptor matches, and passes the payload. An object of another class, or a non-object, produces a `Type` error that names the expected class. A result typed by a descriptor is wrapped as a new object, or becomes null for a null pointer. A plugin therefore never reads memory that is not its own, and never sees the core's object.

## Strings

A string crosses as a `Text`, which is one word: the address of the core string itself. The ABI describes the string's header as `TextData`: the core's own word, the length in bytes, and then the UTF-8 bytes. A `Text` parameter is the string the caller passed. Every language's adapter hands strings over as core strings already, so the plugin borrows it for the call and reads it as a `str`. A value that is not a string produces a `Type` error, the same as a wrong scalar. A `Text` result is one the plugin created with `Text::new`, which asks the core to allocate it. The dispatcher passes the word back as the value it is. Nothing is copied at the crossing, and the plugin never writes a header itself.

## Binary buffers

`caribou_abi::Buffer` is a one-word carrier for a GC-owned buffer. `Buffer::new(&bytes)` copies into the core heap once. A parameter borrows that buffer; returning the parameter passes the same storage back. `get`, `set`, and `len` do not allocate. `to_vec()` explicitly copies into plugin-owned Rust memory. `unsafe as_slice()` borrows without copying: no language may mutate the bytes while that borrow lives, including through a callback.

A buffer may be read-only: one over bytes a language holds immutable, such as a Lua string's, or a string passed where a buffer is taken, which is a view of the string's own bytes. `is_read_only()` says so, `set` refuses it, and `as_mut_ptr()` gives no pointer to write through. A function that writes a buffer takes `BufferMut` for it, not `Buffer`: the call refuses a read-only buffer, or a string, for that parameter before the function runs, and `BufferMut::as_mut_ptr()` is then always writable. `copyOut` in the gpu plugin is one.

Haxe sees `haxe.io.Bytes`. Its wrapper and the core buffer retain the same GC-managed backing allocation, in both directions, so mutations are visible through every view. A crossing allocates a small header, not a copy of the byte contents. Empty buffers work; `Buffer::NULL` represents null rather than an empty allocation. The core traces the backing pointer, and never scans the binary contents as pointers. Store `buffer.value()` in a `Kept` to retain it across plugin calls, and recover it with `Buffer::of`.

The core buffer implements `len`, indexing, indexed writes and iteration. Wren uses its existing foreign sequence view (`count`, `[]`, `[]=` and `for`), keeping the same object and bytes.

## Enums

Declare an enum alongside classes and functions in `plugin!`. The macro generates the ordinary Rust enum and its language-neutral constructor metadata. `Enum<T>` is its one-word C ABI carrier; `.into()` builds a core enum, and `.get()` decodes a borrowed carrier into its Rust enum. Rust enum memory layout never crosses the ABI.

```rust
use caribou_abi::Enum;

pub extern "C" fn event() -> Enum<Event> {
    Event::Resized(800, 600).into()
}

caribou_abi::plugin! {
    name: "example";
    enum Event {
        Closed;
        Resized(width: i32, height: i32);
    }
    fn event() -> Enum<Event>;
}
```

An ordinary Rust enum can instead derive `PluginEnum`. Serde is not required: the derive reads the variants and fields at compile time and generates the same static descriptors and direct conversions as `plugin!`.

```rust
use caribou_abi::{Enum, PluginEnum};

#[derive(PluginEnum)]
#[caribou(name = "example.Event")]
pub enum Event {
    Closed,
    Resized { width: i32, height: i32 },
    Message(String),
}

pub extern "C" fn event() -> Enum<Event> {
    Event::Resized { width: 800, height: 600 }.into()
}

caribou_abi::plugin! {
    name: "example";
    enum Event; // Register the existing enum, without redeclaring it.
    fn event() -> Enum<Event>;
}
```

Unit, tuple and named-field variants work. Named fields retain their names; tuple fields default to `a0`, `a1`, etc. `#[caribou(name = "width")]` on a field overrides its exported name, and the same attribute on a variant changes its constructor name. Generic enums are not supported. Every payload type must implement `EnumField`; the derive supplies that implementation for the enum itself, so nested ordinary Rust enums work too. Register each nested enum with `enum NestedType;` in `plugin!`. Native Rust enums are payloads; function signatures still use the one-word `Enum<T>` carrier.

For a type from another crate, a local enum may also use `#[caribou(from = native::Event)]`. The derive generates `From<native::Event>`: variants and fields match by name and each field converts through `Into`. A variant attribute such as `#[caribou(pattern = native::Event::Resized(size))]` overrides its source pattern; field attributes such as `#[caribou(value = size.width as i32)]` project or transform payloads. The compiler checks source-pattern exhaustiveness. An explicit enum-level `fallback = Self::None` handles unmatched source variants, and `#[caribou(skip)]` excludes a local sentinel from source matching. This mapping is necessary for external types: a derive sees the enum it decorates, not another crate's definition.

An argument a caller may leave out is `Option<Enum<T>>`: `None` when the caller passes null. Haxe types it `Null<example.Event>`. It is an argument only; a function that may return no enum declares a variant for none.

Haxe automatically gets `example.Event`, an actual enum with `Closed` and `Resized(width:Int, height:Int)`. Returned values work in `switch`, and Haxe-created constructors can be passed back to a plugin. Payloads support the ABI numeric scalars (including 64-bit integers), booleans, `String`, `Text`, `Buffer`, `Value`, derived enums, and nested `Enum<T>` carriers. Rust-owned strings copy into the host once when encoded; decoding them produces owned Rust strings. Buffers and existing carriers share their storage. The encoder roots existing host references before allocating and roots each converted field before encoding the next. Queue ordinary Rust enums to defer host allocation until delivery; retain an already encoded carrier with `Kept` across calls.

The core stores a constructor index followed by traced `Value` fields. Constructor metadata belongs to the type and is shared by every instance. A plugin call borrows or returns the core enum directly. Haxe conversion allocates an enum under the loaded program's own type and translates its payload slots; buffer payloads still share their backing bytes. This conversion is not allocation-free. Haxe constructor names, order, arity, and field types are checked against the plugin declaration before the program runs; a stale declaration requires recompiling the Haxe bytecode.

Wren sees a published class for the enum, with `tag`, `constructor`, and named payload getters; for example, `event is Event`, `event.constructor`, and `event.width`. Inactive payload fields are missing, and enums are immutable. The class also makes values: a fieldless variant is a static getter, `Event.Closed`, and a variant with fields is a static method taking them in order, `Event.Resized(640, 480)`. The core checks each field against the declaration, as for any constructor. The plugin publishes these for every language, as static fields of the enum's class object and static methods whose target is the variant's constructor. Wren retains the core enum through the existing foreign object mechanism. The descriptors and protocols are shared core facilities; Zyntax's existing restriction on object and array transfer still applies until that adapter implements those conversions.

The window plugin's enums are generated. `plugins/cb_window` is xwindow's adapter: its build generates the `window` model from xwindow's declaration and installs xwindow's winit and page backends, so `Event` covers all 28 winit 0.30 window events, the raw device events and the application lifecycle, with typed keyboard, IME, modifier, touch-force, path and device payloads. `None` means the queue is empty. See [the window plugin](../../plugins/cb_window/README.md) and xwindow's README for the API. `plugins/fixtures/window` matches every event in Haxe.

The non-window interop fixture `UseData.hx` covers shared storage, mixed and nested payloads, and GC retention. The window-event test plugin generates the same model with a class of probes appended to the declaration, and makes the events a window reports without opening one, so Haxe and Wren read every payload across a collection without a GUI.

These additions change the host and plugin tables. ABI version **2** requires rebuilding existing plugins; the loader rejects version 1 before reading the new layouts.

## The Host Table

`caribou_plugin_entry` receives the core's table, `host::Host`, and the macro stores it. `caribou_abi::host` is the plugin's side of the table: plain functions that the tooling can see. Through it, a plugin can:

* **Create strings** with `text`, buffers with `Buffer::new`, and enums through `Enum<T>`.
* **Call values it was given** with `call`. The `Err` case is the error value the call raised.
* **Raise errors.** `raise` creates an error of a given kind with a message and marks it pending. `raise_value` marks an error value that a call returned as pending. In both cases the plugin function then returns, its result is ignored, and its caller sees the error the same way it sees an error raised by any language.
* **Start its part in a page** with `agent`. See [A Plugin in a Page](#a-plugin-in-a-page). Anywhere else, `agent` returns false.
* **Watch a word** with `watch`. The plugin's handler runs on the world's main context each time the word changes. The call returns the world's wake word, which whatever changes the word from outside the world, such as the plugin's part in a page, adds one to and notifies.
* **Keep values across calls** in a `Kept`. A `Kept` is a handle that the collector honors until the `Kept` is dropped. A bare `Value` stored in the plugin's own memory is invisible to the collector. A plugin object that keeps a callback stores it in a `Kept` and invokes it with `call` when needed. The function can come from Wren or from Haxe, and anything it raises comes back to the plugin to handle or pass on.

Every entry in the table is called on the caller's thread, inside the plugin function the dispatcher is calling. A plugin has no thread of its own to call from.

## A Plugin in a Page

A plugin that runs in a browser can bring its own JavaScript, for the parts only a page can do. For example, the GPU plugin's page part holds the browser's WebGPU. The page itself stays generic: what it knows about any plugin is this convention.

* **What the plugin ships.** The plugin crate's build script writes its page files to `$OUT_DIR/page/`. The entry is `<name>.mjs`, named after the plugin. Any other file there is the plugin's own, loaded by the entry relative to itself. `caribou build` for wasm writes every file in that directory beside the program, whether the plugin is linked or a side module.
* **How it starts.** When the program calls `host::agent(name, address)`, the page imports `./<name>.mjs` on its own thread and calls its exported `start({ memory, address, canvas })`. `memory` is the program's shared `WebAssembly.Memory`, `address` is what the plugin passed, and `canvas` is the page's `<canvas>` element.
* **What it may do.** The entry runs on the page's thread, so it can listen for DOM events and start Workers, which a Worker cannot always do. A part that needs its own event loop, such as the GPU plugin's, runs in a Worker the entry starts. The entry may give that Worker the canvas with `transferControlToOffscreen`; only one part can take it, and whoever takes it then owns the drawing buffer's size.
* **How it talks to the program.** Only through the program's shared memory, at the address it was given. It can wait with `Atomics.waitAsync` and wake the program by notifying. It can also wake the program's world when a word the plugin watches changes, by adding one to the wake word and notifying. It never calls into the program.

The page's own concerns stay the page's. The window, meaning the canvas, its size and its DOM events, is Ash's page's, not a plugin's.

## Hatch Packages

A project's Wren modules depend on hatch packages in the project file's `[dependencies.wren]`, written as a hatchfile writes them. For a `.hl` opened directly, the dependencies are those of a `hatchfile` at a root.

* **Resolution:** The driver resolves each dependency the way `hatch` does (`wren_lift::hatch::resolve_dependency_bytes`): a path dependency is built from its workspace, and a version dependency comes from the cache that `hatch install` fills. Transitive dependencies are resolved as well, each once.
* **Staging:** Each package is staged in the VM the way WrenLift stages it (`stage_hatch_modules`). Its modules wait for their first `import "@hatch:noise"`, its native libraries are registered, and WrenLift opens them itself.
* **Symbol resolution:** A package's plugin calls the host through the `wlift_plugin_*` symbols, which it resolves against the process that opened it. The binaries in this repository export their symbols (`.cargo/config.toml`) so that process can be `caribou`. Nothing in the package, the plugin, or WrenLift knows that Caribou is involved.
* **Bundles:** A bundle carries each package whole, as a module section with format `hatch` under the package's name. A session started from the bundle stages the packages the same way (`caribou_wren::hatch`).

## Current Implementation Boundaries

**Implemented:**

* The header macro, loading, and the adapter.
* Scalar, `DYN`, string, buffer, and enum parameters and results.
* Classes with instances.
* The host table: kept values, calls, and errors.
* Discovery next to the program, and plugins shipped in a bundle.
* Wren and Haxe calling into a plugin.
* Hatch packages for Wren.

**Not yet implemented:**

* Native object, buffer, and enum conversion in the Zyntax adapter.
