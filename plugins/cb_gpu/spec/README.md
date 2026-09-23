# WebGPU specification input for the Caribou GPU plugin

This directory contains the vendored WebGPU IDL used when building
`caribou-gpu`. The plugin exports its API under the **`gpu`** namespace;
`wgpu` is its Rust backend.

`webgpu.idl` was copied from hlwgpu's specification snapshot, recorded there
as fetched on 2026-09-07 from
<https://gpuweb.github.io/gpuweb/webgpu.idl>. It is checked in so builds use
a fixed input and do not fetch the specification over the network.

## How the plugin uses it

[`../gpu.api.rs`](../gpu.api.rs) selects the WebIDL declarations to expose:

```rust
#[idl("GPUBlendFactor")]
enum BlendFactor {}

#[idl("GPUBufferUsage")]
mod BufferUsage {}
```

The `caribou-bindgen` build dependency reads these declarations and generates:

- Caribou enum schemas and Rust enum types, such as `gpu.BlendFactor`.
- Static constant accessors, such as `gpu.BufferUsage.STORAGE()`.
- Typed resource and descriptor wrappers and the `plugin!` export table from
  the declarations in `gpu.api.rs`.

Haxe discovers the resulting types through `-lib caribou`. The same plugin
metadata is available to other Caribou frontends. There is no separate
HashLink primitive table or generated `wgpu` Haxe package.

## Scope

The IDL is a source for enum values and constants, and a reference for future
API coverage. Its presence does **not** mean the plugin implements every
WebGPU operation or descriptor. Resource methods are explicitly declared in
`gpu.api.rs` and implemented in [`../src/backend.rs`](../src/backend.rs).
Texture and vertex formats currently expose the subset listed in that API
declaration.

The previous hlwgpu coverage figures do not describe this plugin. See the
[GPU plugin README](../README.md) for the supported operations, Caribou types,
resource lifetime rules and runnable compute example.

## Updating the snapshot

Replace `webgpu.idl` deliberately and record its source date or revision here.
Review changes to the selected enum ordering against the backend mappings,
update the API declaration and native implementations together, then run the
binding-generator and GPU-plugin tests. The build reads this file; it never
rewrites it.
