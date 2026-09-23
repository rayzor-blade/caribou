# Native plugin fixtures

Each fixture has a Rust runner in `src/bin`, a Haxe project and a `plugins/`
directory containing its native library.

```sh
cargo build -p caribou-plugin-fixtures --bins
cargo run -p caribou-plugin-fixtures --bin gpu
cargo run -p caribou-plugin-fixtures --bin window
cargo run -p caribou-plugin-fixtures --bin window_gpu
```

The GPU runner executes the compute/readback checks and needs access to a
native GPU. The window runner opens an interactive window and logs events
until the window closes. The `window_gpu` runner uses both plugins to render
a triangle into a Caribou window. Pass a frame count to make that example
exit automatically, for example `cargo run -p caribou-plugin-fixtures --bin
window_gpu -- 3`. All three use the hybrid runtime mode.

`build.rs` builds the native plugins and a matching `caribou` descriptor
command in an isolated target directory under `OUT_DIR`. It copies each
plugin to its fixture and compiles every Haxe project. The combined fixture's
`window_gpu/plugins` directory therefore contains both native libraries so a
single Caribou session discovers both namespaces. Haxe and haxelib must
be available on `PATH`; Caribou's Haxe library is registered in a build-local
haxelib repository, without changing the user's global development mapping.

The build script lists each fixture's directory, Cargo package and Rust
library name explicitly in `FIXTURES`. Add a new runner under `src/bin` and
an entry there when adding another fixture. Only source inputs are watched;
rewriting `.hl` files or copied libraries does not retrigger the build.
