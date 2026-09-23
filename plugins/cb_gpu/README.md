# GPU plugin

`caribou-gpu` is a native Caribou plugin for GPU compute and rendering through
Rust's `wgpu` backend. Its public namespace is **`gpu`**, matching the
`window` plugin's naming convention.

The API includes adapter/device creation, buffers and asynchronous readback,
compute pipelines and dispatch, textures and samplers, render-pipeline
builders, render commands, and window surfaces. Resources cross the boundary
as typed Caribou objects; strings use `Text`, binary data uses shared `Buffer`
storage, and choices such as power preference use Caribou enums.

A Haxe program uses `-lib caribou` and places the plugin library in the
`plugins/` directory beside its HashLink program:

```haxe
import gpu.GpuInstance;
import gpu.Power;
import gpu.BufferUsage;

var instance = new GpuInstance();
var adapter = instance.requestAdapter(HighPerformance);
if (!adapter.valid()) throw "No GPU adapter available";
var device = adapter.requestDevice();
if (!device.valid()) throw "Could not open the GPU device";
var buffer = device.createBuffer(1024,
    BufferUsage.STORAGE() | BufferUsage.COPY_DST());

// Use the buffer, then release native resources explicitly.
buffer.destroy();
device.destroy();
adapter.destroy();
instance.destroy();
```

Returned resource types are inferred. The complete compute/readback example
is [`../fixtures/gpu/src/Main.hx`](../fixtures/gpu/src/Main.hx).

`gpu.api.rs` declares the supported API. `build.rs` passes it and the vendored
WebGPU IDL to `caribou-bindgen`, generating the Rust wrappers, enum schemas
and `plugin!` exports into Cargo's `OUT_DIR`. The native implementation lives
in `src/backend.rs`. Normal builds never rewrite source files.

## How a declaration reaches the backend

```rust
trait GpuDevice {
    #[native(shader_create)]
    fn createShader(this: &GpuDevice, wgsl: Text) -> Box<GpuShader>;
}
```

This is a build-time declaration, not a Rust trait used for dynamic dispatch.
It generates a `GpuDevice` resource wrapper, an `extern "C"` method calling
`backend::shader_create(this.handle, wgsl)`, and the matching `plugin!` export.
The returned native handle is wrapped in `Box<GpuShader>`, which Caribou owns
as an ordinary plugin object. Haxe discovers `gpu.GpuDevice.createShader`
and its inferred `gpu.GpuShader` result through `-lib caribou`. No GPU-specific
Haxe extern library or HashLink primitive table is involved.

Argument lowering is explicit:

| Declaration | Backend argument |
| --- | --- |
| `&GpuBuffer`, `&GpuDevice`, etc. | The resource's private native handle |
| `Text` | The same Caribou UTF-8 string view |
| `Buffer` | The same Caribou shared byte storage |
| `Enum<Power>`, etc. | The enum's declared native code |
| Scalar | The same scalar |

Return lowering wraps native handles in the declared resource class and
native enum codes in actual Caribou enums. It does not expose enum ordinals
as the language-side representation. Generated wrappers translate unwinding
Rust panics into Caribou runtime errors.

To add an operation, implement it in `src/backend.rs` and add one signature
with `#[native(function_name)]` to `gpu.api.rs`. Rust checks the generated call
against the backend signature. The generated `plugin!` table carries argument
and return class identities for every frontend.

## What comes from WebIDL

```rust
#[idl("GPUBlendFactor")]
enum BlendFactor {}
#[idl("GPUBufferUsage")]
mod BufferUsage {}
```

These import enum strings and numeric constants from `spec/webgpu.idl`.
Enum names become Caribou constructors such as `OneMinusSrcAlpha`.
Constants become static methods such as `gpu.BufferUsage.STORAGE()` so they
are available through the same plugin metadata in every frontend.

The generator supports fieldless enums, integer constant namespaces and
explicit resource method declarations. It does **not** infer wgpu operations,
WebIDL dictionary layouts, overloads or Promise scheduling from interfaces.
As in hlwgpu, the native implementation and its API projections remain
explicit. Texture and vertex formats currently expose the backend's declared
subset in `gpu.api.rs`; this is not the complete browser WebGPU API.

## Ownership and calls

Resources are typed Caribou objects around a private native table. Call
`destroy()` when finished; collection frees the wrapper, while explicit
resource destruction controls native/GPU memory. Dependent GPU objects hold
wgpu's own references. Repeated destruction is harmless and `valid()` checks
whether the handle still resolves. Encoders and pipeline builders are consumed
by `submit()` and `build()` respectively.

`requestAdapter()` and `requestDevice()` settle synchronously on this native
backend and return the actual resource. Their request tickets never escape
as adapter or device IDs. Buffer mapping and queue completion return a
`GpuRequest`; poll `ready()` and collect its integer success result once with
`result()`. `destroy()` discards a request. Native callbacks retain Rust state,
never an unrooted Caribou value or a borrowed buffer.

Shader sources and immediate labels borrow `Text.as_str()`. Pipeline builder
entry names are copied because they survive the call. Uploads borrow
`Buffer` storage without an intermediate byte allocation; readback copies
GPU mapped bytes directly into the caller's shared buffer. Supplied lengths
are checked before access. GPU transfers still perform the copies required
by wgpu. `GpuBindings` takes typed buffers, texture views and samplers, so
callers do not pack native handles into byte arrays.

Surfaces accept the platform and raw-handle components supplied by the window
plugin. Keep that window alive until the surface is destroyed. Rendering,
surface presentation and platform support follow wgpu; only the native
backend is implemented here. Building without `native` is intentionally
rejected until a Caribou host forwarding implementation exists.

## Example and checks

`plugins/fixtures/gpu/src/Main.hx` creates a device, uploads four integers,
runs a compute shader, reads the results into `haxe.io.Bytes`, and checks
bounds errors and resource destruction. Returned object types are inferred.

```sh
cargo test -p caribou-bindgen -p caribou-gpu --offline
cargo build -p caribou-gpu -p caribou-driver --offline
```

Copy the platform's `caribou_gpu` library from `target/debug` into
`plugins/fixtures/gpu/plugins`, then run `haxe gpu.hxml` from the fixture
directory. From the repository root:

```sh
target/debug/caribou run plugins/fixtures/gpu/gpu.hl
```

The fixture requires an available GPU adapter and fails explicitly if none
is available. The Rust generator/catalog/handle tests require no GPU.

The compute fixture has been exercised on Apple M1 Pro/Metal, normally and
with `ASH_GC_STRESS=1 WLIFT_GC_STRESS=1`. Surface presentation, rendering and
other platforms have not been exercised by this fixture.

Frontend limitations remain tracked in git-bug: Wren's general enum
constructors and lossless 64-bit integers (`80d0ccf`), and Zyntax's shared
object/buffer/enum transfer (`c575125`). The generated signatures keep the
Caribou types rather than changing the public API around those gaps.

The backend is adapted from the sibling hlwgpu repository at
`3c30f886b809b7823382e343abaa92ede3ef6af4`; its MIT notice is in
`LICENSE.hlwgpu`. HashLink UTF-16 helpers, allocations and primitive tables
are not used. Builds do not depend on that sibling checkout.
