# GPU plugin

`caribou-gpu` is a native Caribou plugin for GPU compute and rendering through
Rust's `wgpu` backend. Its public namespace is **`gpu`**, matching the
`window` plugin's naming convention.

The API covers WebGPU and wgpu's own features beyond it: adapters and devices
with their features and limits, buffers and mapping, textures, samplers,
explicit layouts, compute and render pipelines, passes, queries, render
bundles, copies, error scopes, device loss, surfaces, binding arrays,
external textures, mesh shaders and ray queries. Resources cross the boundary
as typed Caribou objects; strings use `Text`, binary data uses shared `Buffer`
storage, and choices such as power preference use Caribou enums.

A Haxe program uses `-lib caribou` and places the plugin library in the
`plugins/` directory beside its HashLink program:

```haxe
import gpu.GpuInstance;
import gpu.GpuBufferDescriptor;
import gpu.GpuDeviceDescriptor;
import gpu.Power;
import gpu.BufferUsage;
import gpu.Limit;

var instance = new GpuInstance();
var adapter = instance.requestAdapter(HighPerformance).await();
if (!adapter.valid()) throw "No GPU adapter available";
var requirements = new GpuDeviceDescriptor();
requirements.addRequiredLimits(MaxBindGroups, 4);
var device = adapter.requestDeviceWith(requirements).await();
if (!device.valid()) throw "Could not open the GPU device";
var descriptor = new GpuBufferDescriptor(1024,
    BufferUsage.STORAGE() | BufferUsage.COPY_DST());
var buffer = device.createBuffer(descriptor);

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

WebIDL-style dictionaries are declared as Rust structs in `gpu.api.rs`:

```rust
#[idl("GPUBufferDescriptor")]
struct GpuBufferDescriptor {}
```

Bindgen imports dictionary inheritance, required members, defaults, typedefs
and sequences, then emits a plugin-owned Caribou class. Required scalar fields
become constructor arguments, fields with defaults become setters, and
sequences become `addField(T)` methods. The declaration can also spell out
the same shape as Rust fields when it needs a deliberate projection instead
of the complete IDL dictionary.

A WebIDL union is declared as a Rust enum whose variants each carry one
declared type, and a record field of that type gets one setter per variant:

```rust
#[idl("GPUBindingResource")]
enum BindingResource {
    Sampler(GpuSampler),
    TextureView(GpuTextureView),
    Buffer(GpuBuffer),
    BufferBinding(GpuBufferBinding),
}
```

`GPUBindGroupEntry.resource` is typed `GPUBindingResource`, so
`GpuBindGroupEntry` gets `resourceSampler(sampler)`,
`resourceBufferBinding(range)` and so on. Bindgen checks each variant against
the typedef's alternatives; leaving one out is a deliberate projection. A
required union is not a constructor argument, and the backend refuses a
record whose union was never set. A sequence of unions gets
`addField<Variant>` methods. The union is a Rust type only: languages see
the setters, not a dynamic value.

WebIDL members named with a Rust keyword keep their name. The buffer and
sampler binding layouts' `type` member is `layout.type(Storage)` in Haxe.

WebIDL `record<K,V>` members become typed `addField(key, value)` methods and
plugin-owned entry vectors. Nullable sequence elements add both
`addField(value)` and `addFieldNull()`. String and buffer entries retain the
underlying Caribou value instead of copying it.

A resource method borrows the generated Rust record directly, so the
descriptor is not serialized or lowered through a dynamic map. Scalars
remain inline, enums are converted to their native codes, resource fields
retain their typed handles, and nested records copy only descriptor metadata.
Retained `Text` and `Buffer` fields hold Caribou GC roots: text keeps its host
string and buffers keep sharing their original backing bytes without a copy.

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

wgpu has members that WebGPU lacks. `#[extension]` adds one to an imported
declaration: a record field, an enum value after the IDL's values, or a union
alternative. A `mod` can declare its own constants beside the imported ones:

```rust
#[idl("GPUAddressMode")]
enum AddressMode {
    #[extension]
    ClampToBorder,
}
#[idl("GPUBufferUsage")]
mod BufferUsage {
    const BLAS_INPUT: i32 = 1024;
    const TLAS_INPUT: i32 = 2048;
}
```

The generator supports fieldless enums, integer constant namespaces,
dictionary records, declared unions and explicit resource method
declarations. A method can name an IDL operation with
`#[idl("Interface.operation")]`; Promise returns are checked against
Caribou's shared `Future<T>` carrier. It does **not** yet project callback
types, union-typed method parameters, or infer wgpu operations, overloads or
scheduling from interfaces.
As in hlwgpu, the native implementation and its API projections remain
explicit.

## Ownership and calls

Resources are typed Caribou objects around a private native table. Call
`destroy()` when finished; collection frees the wrapper, while explicit
resource destruction controls native/GPU memory. Dependent GPU objects hold
wgpu's own references. Repeated destruction is harmless and `valid()` checks
whether the handle still resolves. Encoders and pipeline builders are consumed
by `submit()` and `build()` respectively.

`requestAdapter()` and `requestDevice()` return typed `caribou.Future<T>`
values, as do buffer mapping and queue completion. `await()` parks the current
Caribou task and preserves the concrete adapter or device result type without
a language-side annotation. Rejected operations raise their wgpu error.
Native platforms drive asynchronous work on short-lived workers, while
browser WebGPU uses its event loop. Completion callbacks retain a rooted
Future rather than an unrooted Caribou value or a borrowed buffer.

## Layouts and bind groups

`createBindGroupLayout`, `createPipelineLayout`, `createBindGroup` and
`createComputePipeline` take the WebGPU descriptors. A layout entry holds
exactly one of `buffer`, `sampler`, `texture`, `storageTexture`,
`externalTexture` or `accelerationStructure`; a buffer layout can take a
dynamic offset and a minimum binding size. A `GpuBufferBinding` binds a range of a buffer, and a texture
binds its default view. `GpuPipeline.getBindGroupLayout(i)` returns an
inferred or explicit layout that other pipelines can share. The render
pipeline builder's `layout(pipelineLayout)` replaces its inferred layout.

`GpuProgrammableStage` names a shader module, an entry point and
pipeline-overridable constants (`addConstants("bias", 1)`). A compute
pipeline descriptor with no `layout` is WebGPU's `"auto"`.

A compute pass stays open from `computeBegin()` to `computeEnd()`, as a
render pass does from `passBegin()` to `renderEnd()`, so one pass can bind
several groups and dispatch several times. Dynamic offsets use WebGPU's
`Uint32Array` form: `computeSetBindGroupOffsets(index, group, offsets,
start, count)` reads `count` 32-bit offsets from element `start` of a shared
buffer, checked against its length first. `renderSetBindGroupOffsets` is
the render pass equivalent. `compute()` remains the one-dispatch shorthand.

The fixture's `explicitLayouts` binds a dynamic storage window and a uniform
range, overrides a constant, and dispatches one bind group at two offsets.

## Shaders in HXSL

A Haxe program can write its shaders in HXSL, the shader language of Heaps.
A class that implements `caribou.hxsl.Shader` declares its source in
`static var SRC`, and the build macro checks it when the program compiles:
a type error is a compile error at its line in the shader. The macro then
replaces `SRC` with `static inline var WGSL`, the shader as WGSL.

```haxe
class TintedQuad implements caribou.hxsl.Shader {
	static var SRC = {
		@input var input : { position : Vec2, uv : Vec2 };
		var output : { position : Vec4, color : Vec4 };
		@param var tint : Vec4;
		@param var picture : Sampler2D;
		var uv : Vec2;
		function vertex() {
			output.position = vec4(input.position, 0, 1);
			uv = input.uv;
		}
		function fragment() {
			output.color = picture.get(uv) * tint;
		}
	};
}

var shader = device.createShader(TintedQuad.WGSL);
```

The WGSL has a `vertex` and a `fragment` entry point, or `main` for a
compute shader. `output.position` is the vertex position, and each other
field of `output` is a color target, in declaration order. The macro also
adds constants for everything the program binds, so a misspelled name is a
compile error too:

- `PARAMS_SIZE` and `PARAM_tint`: params and globals share one uniform
  buffer at group 0, binding 0, laid out by WGSL's uniform rules; each
  constant is a byte offset. A `Bool` param is a 32-bit integer there.
- `TEXTURE_picture`: a texture's binding; its sampler is the next one.
- `BUFFER_values`: the binding of a buffer or storage texture.
- `INPUT_position`: a vertex attribute's location, in declaration order.
- `TARGET_color`: a color target.
- `CONST_steps`: the pipeline-constant key of a `@const`, which is a WGSL
  override.

`@:import Other;` brings another shader's variables and helper functions
into this one, and a class with only helpers is a module other shaders
import; it prints no WGSL. `@:extends Base;` takes another shader's stages
too, and a function declared again replaces the inherited one.

Arrays of textures and atomics are not printed yet. Heaps' channels, bindless
resources and barycentrics have no WGSL here.

The checker, evaluator, linker, stage splitter and dead-code pass under
`haxe/caribou/hxsl` are Heaps' own, from `9c51d45f`, under its MIT notice
in `LICENSE.heaps`; `WgslOut` and the build macro are Caribou's.

## Pipelines and passes

`createRenderPipeline` takes WebGPU's render pipeline descriptor: vertex
buffers and attributes, primitive, depth-stencil and multisample state, and a
fragment stage with its targets and blending. Each stage takes its own module,
entry point and constants. `createComputePipelineAsync` and
`createRenderPipelineAsync` compile on a worker and return a
`Future<GpuPipeline>`.

`beginRenderPass` and `beginComputePass` take the pass descriptors. A color
attachment takes a texture or a view, a resolve target, a load and a store
operation and a clear value. A depth-stencil attachment takes the same per
aspect, or read-only flags. wgpu's `DontCare` load leaves an attachment
undefined until the pass writes it; a device allows it only with
`dontCareLoads(true)` on its descriptor. Passes can write timestamps and hold an occlusion
query set. Inside a render pass, draws take full ranges; `renderMultiDraw*`
issue several indirect draws, and the `Count` forms read the count from a
buffer. Immediate data, occlusion queries, pipeline statistics queries and
render bundles are recorded the same way.

`createQuerySet` makes occlusion, timestamp and pipeline statistics queries,
and `resolveQuerySet` writes their results into a buffer at a 256-byte
aligned offset. `queue.timestampPeriod()` gives nanoseconds per tick.
`createRenderBundleEncoder` records draws once for any number of passes.
Copies between buffers and textures take WebGPU's full copy descriptions:
mip level, origin, aspect, buffer layout and extent. `queue.writeTextureWith`
uploads through the same layout.

## Shaders wgpu does not check

`createShaderModule` takes WGSL with wgpu's runtime checks, each on unless
the descriptor turns it off: bounds checks, loop bounding, ray query and
task dispatch tracking, mesh index clamping and integer division checks.
`createShaderPassthrough` hands backend code to the driver as it is: SPIR-V
on Vulkan, DXIL or HLSL on DX12, a metallib or MSL on Metal, WGSL in the
browser, with the `PassthroughShaders` native feature. Neither is
validated, so both need `trustedShaders(true)` on the device descriptor:
the program's promise that its code is valid for the backend, stays in
bounds and terminates. A passthrough compute pipeline needs an explicit
layout; Metal numbers buffers in the layout's order.

## Pipeline caches

With the `PipelineCache` native feature (Vulkan), `createPipelineCache`
makes a cache that pipelines add to when their descriptor names it.
`getData()` returns its bytes to save, and `adapter.pipelineCacheKey()`
names the adapter they belong to. A cache made with saved `data` starts
from them; wgpu rejects data from another adapter, driver or wgpu version,
starting an empty cache instead unless `fallback(false)` makes that an
error. It cannot tell damaged or forged bytes from real ones, so loading
data needs `pipelineCacheData(true)` on the device descriptor.

## Errors and loss

`pushErrorScope(filter)` and `popErrorScope()` catch validation,
out-of-memory and internal errors; the popped future resolves null when
nothing went wrong. wgpu keeps scopes per thread, so pop from the task that
pushed. Errors outside any scope queue up for `takeError()`. `device.lost()`
resolves with a reason and a message, `Destroyed` after `destroy()`.
`getCompilationInfo()` returns a shader's messages with their line, column,
offset and length.

## Capabilities

Adapters and devices expose `supports(Feature)` and `limit(Limit)`. Both
catalogs are generated from the vendored WebGPU IDL. A
`GpuDeviceDescriptor` collects required features and limits, and
`requestDeviceWith()` passes them to wgpu device creation. Values below the
WebGPU default are ignored as required by the specification; values outside
the adapter's capability reject the returned Future. Draft WebGPU features or
limits absent from wgpu 30 report unsupported instead of being silently
enabled. `requestDevice()` remains the default-capability convenience call.

wgpu's own features and limits are `NativeFeature` and `NativeLimit`, queried
with `supportsNative` and `nativeLimit` and requested through
`requiredNativeFeatures` and `requiredNativeLimits`. `NativeFeature` is
generated from wgpu's feature flags. Binding arrays, mesh shaders and ray
queries all default to limits of zero, so a device that uses them requests
limits as well as features. `textureFormatFeatures` and
`textureFormatUsages` report what the adapter allows for each texture format.

wgpu marks some features `EXPERIMENTAL_*`: mesh shaders and ray queries among
them. wgpu warns that these may still have bugs that are undefined behaviour.
A program accepts that with `experimentalFeatures(true)` on the device
descriptor, wgpu's own opt-in for that device and no other. Requesting an
experimental feature without it rejects the device request.

`GpuInstance.createWith` chooses backends and instance flags, and
`requestAdapterWith` takes a power preference, a fallback flag and a surface
the adapter must be able to present to. A device descriptor can take memory
hints. Adapters report their PCI ids, device type and subgroup sizes; buffers
and textures report their size, shape, format and usage.

## wgpu's own features

A layout entry with a `count` is a binding array. Its bind group entry takes
`resourceBufferArray`, `resourceSamplerArray`, `resourceTextureViewArray` or
`resourceAccelerationStructureArray`.

`createExternalTexture` makes an external texture from one to three planes
(RGBA, NV12 or YU12), with the conversion matrices and transfer functions
that sampling applies. It binds to WGSL's `texture_external`.

`createMeshPipeline` takes an optional task stage and a mesh stage in place of
vertex input, and `renderDrawMeshTasks` and its indirect forms draw with it.

Ray queries search acceleration structures. `createBlas` makes a bottom-level
structure for triangles or boxes, and `createTlas` a top-level one of up to
`maxInstances` instances. `setInstance` places a BLAS with a transform, custom
data and a mask. `buildAccelerationStructures` builds both kinds on an
encoder, a TLAS binds like any other resource, and `prepareCompaction` with
`queue.compactBlas` shrinks a built BLAS.

Shader sources and immediate labels borrow `Text.as_str()`. Pipeline builder
entry names are copied because they survive the call. Uploads borrow
`Buffer` storage without an intermediate byte allocation; readback copies
GPU mapped bytes directly into the caller's shared buffer. Supplied lengths
are checked before access. GPU transfers still perform the copies required
by wgpu. `GpuBindings` takes typed buffers, texture views and samplers, so
callers do not pack native handles into byte arrays.

Surfaces accept the platform and raw-handle components supplied by the window
plugin. Keep that window alive until the surface is destroyed. The backend
recognises AppKit, Win32, Xlib, Wayland, Android NDK, UIKit, HTML canvas and
offscreen-canvas handles. A frontend still has to provide the corresponding
handle; the current window plugin produces the four desktop forms.

The same Rust implementation compiles against Metal on Apple platforms,
DX12 on Windows, Vulkan on desktop Unix, Vulkan/GLES on Android, browser
WebGPU on `wasm32-unknown-unknown`, and GLES on Emscripten. This keeps the
plugin ABI and resource model buildable for Caribou's future wasm runtime.
Browser execution is not complete yet: Caribou must statically register the
plugin and provide an HTML or offscreen canvas handle. The plugin ABI already
preserves typed Promise results, and the GPU requests use a local browser task
instead of blocking the event loop.

Buffer sizes, ranges, offsets and adapter limits use 64-bit integers, matching
WebGPU's `GPUSize64`. Dimensions, counts, flags and shared-buffer lengths use
32-bit integers.

## What is not exposed

- WebGPU members wgpu 30 does not have: texture component swizzle, texture
  binding view dimension, a buffer's map state, and reading a label back.
- Backend handle interop, which has no portable form.
- API tracing, which needs a wgpu build feature.
- Browser image, canvas and video sources, which need the browser runtime.
- The three-element sequence spelling of a texture extent; `GpuExtent3D` is
  the dictionary spelling.

Features remain conditional on the adapter and backend that implement them.
Compiling on a platform does not mean the platform has been run.

## Example and checks

`plugins/fixtures/gpu/src/Main.hx` creates a device, uploads four integers,
runs a compute shader, reads the results into `haxe.io.Bytes`, and checks
bounds errors and resource destruction. Further parts run explicit layouts,
HXSL render and compute shaders, rendering with queries and bundles, error scopes and device loss, then binding
arrays, an external texture, a ray query and a mesh shader where the adapter
has them, and the introspection calls. `plugins/fixtures/window_gpu` checks
surface capabilities and configuration and presents frames. Returned object
types are inferred.

```sh
cargo test -p caribou-bindgen -p caribou-gpu --offline
cargo build -p caribou-gpu -p caribou-driver --offline
cargo check -p caribou-gpu --target wasm32-unknown-unknown
```

Copy the platform's `caribou_gpu` library from `target/debug` into
`plugins/fixtures/gpu/plugins`, then run `haxe gpu.hxml` from the fixture
directory. From the repository root:

```sh
target/debug/caribou run plugins/fixtures/gpu/gpu.hl
```

The fixture requires an available GPU adapter and fails explicitly if none
is available. The Rust generator/catalog/handle tests require no GPU.

Both fixtures have been run on Apple M1 Pro/Metal, the GPU fixture also with
`ASH_GC_STRESS=1 WLIFT_GC_STRESS=1`. On Intel Iris Xe/Vulkan (Mesa 26.0.3)
the GPU fixture's layouts, trusted shaders and pipeline caches pass, and its
occlusion check fails: wgpu itself reports no samples there.

Wren builds the generated enums through their classes, such as
`TextureFormat.Rgba8unorm`. Zyntax's shared object, buffer and enum transfer
is tracked in git-bug `c575125`. The generated signatures keep the Caribou
types rather than changing the public API around that gap.

The backend is adapted from the sibling hlwgpu repository at
`3c30f886b809b7823382e343abaa92ede3ef6af4`; its MIT notice is in
`LICENSE.hlwgpu`. HashLink UTF-16 helpers, allocations and primitive tables
are not used. Builds do not depend on that sibling checkout.
