# Plugins

A plugin is native code every hosted language reaches the same way: a
dynamic library on the shared ABI (`caribou_abi`), whose table names its
functions, the class each hangs in, and their signatures. A language sees
of a plugin what the driver loaded and nothing more; no language reaches
a plugin by a path of its own.

## Writing one

A plugin's crate depends on `caribou_abi` and nothing else of caribou,
and is a `cdylib`. The whole of it is one `plugin!` invocation beside
the code it calls:

```rust
caribou_abi::plugin! {
    name: "math";
    fn hypot(a: f64, b: f64) -> f64 { a.hypot(b) }
    fn twice(n: i32) -> i32 { n * 2 }
    fn same(v: Value) -> Value { v }
    class Vec {
        fn len3(x: f64, y: f64, z: f64) -> f64 { (x * x + y * y + z * z).sqrt() }
    }
}
```

Each `fn` is written as in Rust and becomes a C function of that
signature; a `class` groups the functions that hang in one class, and a
function outside any class is a static of a class named after the
plugin (`Math`). The macro reads each type's tag off the `Tagged` trait
(`u8`, `u16`, `i32`, `i64`, `f32`, `f64`, `bool`, `()` and `Value`) and
writes the `SymbolDesc` table, the `PluginInfo`, and the two symbols
every plugin exports: `caribou_abi_version`, which a core compares with
its own before it binds anything, and `caribou_plugin_entry`, which
returns the table. Function names are unique across a plugin, since
each is a Rust function.

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

Built: the macro, loading, the adapter, scalar and `DYN` parameters and
results, discovery beside the program, Wren reaching a plugin. Not
built: plugin objects (`TypeTag::OBJ`: a native payload a core cell
holds through `unwrap_native`, with a finalizer), strings and bytes
(`BYTES`), the Haxe side at build time (the macro describing a plugin
and emitting its externs), plugins from a bundle's native library
sections, and wren_lift's own plugins on this ABI.
