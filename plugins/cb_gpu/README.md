# GPU plugin compatibility package

`caribou-gpu` is xgpu's Caribou runtime adapter. It owns the Caribou ABI
objects, plugin entry point, and implicit Haxe, Wren, and Zyntax externs while
using the API model and resource core from
[`xgpu`](https://github.com/rayzor-blade/xgpu).

xgpu owns the `gpu` API declaration, WebGPU and canvas IDL, binding and wire
generators, resource handle core, and native/browser backend operations. This
package supplies Caribou's text, buffer, future, object, error, and heap
conventions around that shared model.

The sibling revision is pinned in the workspace `Cargo.toml` and cloned by the
CI sibling action. The local `[patch]` entry makes development use `../xgpu`
at that revision.

The GPU and combined window/GPU fixtures remain under `plugins/fixtures`:

```sh
cargo test -p caribou-plugin-fixtures --no-run
cargo run -p caribou-plugin-fixtures --bin gpu
cargo run -p caribou-plugin-fixtures --bin window_gpu
```
