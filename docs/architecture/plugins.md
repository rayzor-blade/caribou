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

pub struct Vec;
impl Vec {
    pub extern "C" fn len3(x: f64, y: f64, z: f64) -> f64 { (x * x + y * y + z * z).sqrt() }
}

caribou_abi::plugin! {
    name: "math";
    fn hypot(f64, f64) -> f64;
    fn same(Value) -> Value;
    class Vec {
        fn len3(f64, f64, f64) -> f64;
    }
}
```

A function outside any class is a static of a class named after the
plugin (`Math`). Each declaration is checked against the item it names,
as a coercion to the declared function pointer type, so a signature
that drifts does not compile. The macro reads each type's tag off the
`Tagged` trait (`u8`, `u16`, `i32`, `i64`, `f32`, `f64`, `bool`, `()`
and `Value`) and writes the `SymbolDesc` table, the `PluginInfo`, and
the two symbols every plugin exports: `caribou_abi_version`, which a
core compares with its own before it binds anything, and
`caribou_plugin_entry`, which returns the table.

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

## Boundaries of the current implementation

Built: the header macro, loading, the adapter, scalar and `DYN` parameters and
results, discovery beside the program, Wren reaching a plugin. Not
built: plugin objects (`TypeTag::OBJ`: a native payload a core cell
holds through `unwrap_native`, with a finalizer), strings and bytes
(`BYTES`), the Haxe side at build time (the macro describing a plugin
and emitting its externs), plugins from a bundle's native library
sections, and wren_lift's own plugins on this ABI.
