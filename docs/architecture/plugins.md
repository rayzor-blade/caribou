# Plugins

A plugin is native code every hosted language reaches the same way: a
dynamic library on the shared ABI (`caribou_abi`), whose table names its
functions, the class each hangs in, and their signatures. A language sees
of a plugin what the driver loaded and nothing more; no language reaches
a plugin by a path of its own.

## Writing one

A plugin's crate depends on `caribou_abi` and nothing else of caribou,
and is a `cdylib`. Its functions are ordinary Rust items, `extern "C"`
over the types the ABI tags cover, and a class is a type whose
associated functions hang in it; so the crate reads, and the tooling
sees it, as any Rust module does. One `plugin!` invocation then names
what is exported, by signature, the way a header declares it:

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

A function outside any class is a static of a class named after the
plugin (`Math`). Each declaration is checked against the item it names,
as a coercion to the declared function pointer type, so a signature
that drifts does not compile. The macro reads each type's tag off the
`Param` and `Returned` traits: `u8`, `u16`, `i32`, `i64`, `f32`, `f64`,
`bool`, `()` and `Value` by their tags; `&T` and `&mut T` of a declared
class as an object of it, borrowed for the call, which makes the
function an instance method when it is the first parameter; `Box<T>` as
a new object of the class, owned by the core from then on. A static
`new` returning its own class is the class's constructor. The macro
writes the `SymbolDesc` table, the class table with a finalizer per
class (the `Box` dropped), the `PluginInfo`, and the two symbols every
plugin exports: `caribou_abi_version`, which a core compares with its
own before it binds anything, and `caribou_plugin_entry`, which returns
the tables.

## Loading

`caribou_plugin::load` opens a library, refuses one of another ABI
version, and reads its table; `load_dir` opens every library in a
directory that exports the entry, passing over the ones that do not.
The driver loads the plugins in `plugins/` beside what it opens, a
program or a bundle, before the program starts: the project's layout is
the configuration.

## A language of its own

The plugin adapter (`caribou_plugin::Runtime`) registers one language
per plugin, named after it. So every plugin has a namespace with nothing
configured, and a Wren program writes `import "math:Math" for Math` the
way it imports any language's class; a Haxe program will write `import
math.Math` once the build macro describes plugins.

The table publishes to the registry as one module per class, named
after the class, with the class's functions as its methods. Every target
is a `Callable::Typed` whose signature is an `hl_type` of kind `HFUN`
built from the tags, one object per distinct signature for the process,
and the plugin's language has a typed dispatcher: it reads the
signature's kinds, passes each `Value` as the word of its kind, an
integer as itself, a float as its bits, a bool as 0 or 1, a `DYN` as the
value's bits, calls through `ash_native_call`'s generated table, and
reads the result back the same way. Nothing is boxed. A value a kind
cannot take is a `Type` error naming the argument; a signature the table
does not cover is an error too.

## Objects

An instance of a plugin class is a core object of the class's
descriptor, two words: the descriptor and the payload the constructor's
`Box` gave. The descriptor, one per class for the process, is the
class's identity: it carries the class's type name (`math.Vec2`), so a
language installs its class for the object as it does for any
published type, and Wren's `Vec2.new(3, 4)` gives an instance that
adopts the object and `v is Vec2` holds; its drop hook runs the
plugin's finalizer when the object dies, on the sweep, so the payload
lives exactly as long as anything holds the object. The object's
protocol answers its type name, identity for `equals` and `hash`, and
`unwrap_native` with the payload; the plugin's own functions are what
reach it, through the classes the languages installed.

In a signature an object parameter or result is typed by the
descriptor itself, which is an `hl_type` at word zero: the dispatcher
sees a descriptor where a scalar kind would be, takes the argument's
object, through the cell another language holds it by, checks that its
descriptor is that one, and passes the payload; an object of another
class, or no object, is a `Type` error naming the class. A result that
is a descriptor's is wrapped as a new object, or null for a null
pointer. So a plugin never reads memory that is not its own, and never
sees the core's object.

## Boundaries of the current implementation

Built: the header macro, loading, the adapter, scalar and `DYN`
parameters and results, classes with instances, discovery beside the
program, Wren reaching a plugin. Not built: strings and bytes (`BYTES`),
the Haxe side at build time (the macro describing a plugin and emitting
its externs), a host API a plugin keeps a core value through across
calls, plugins from a bundle's native library sections, and wren_lift's
own plugins on this ABI.
