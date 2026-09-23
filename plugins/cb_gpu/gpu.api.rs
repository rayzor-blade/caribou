// This declaration generates object wrappers and the entire plugin! table.
// Native functions live in src/backend.rs. Enum ordinals are backend values,
// not the layout of Caribou enums. WebIDL is vendored for reproducible builds.
enum Power { None = -1, LowPower = 0, HighPerformance = 1 }
enum Backend { Noop = 0, Vulkan = 1, Metal = 2, Dx12 = 3, Gl = 4, BrowserWebGpu = 5 }
enum Limit { MaxTextureDimension1D, MaxTextureDimension2D, MaxTextureDimension3D, MaxBindGroups, MaxBufferSize, MaxComputeWorkgroupSizeX, MaxComputeInvocationsPerWorkgroup }
// These formats are the subset supported by the imported backend.
#[idl("GPUTextureFormat")]
enum TextureFormat { Unknown = -1, Rgba8Unorm = 0, Bgra8Unorm = 1, Rgba8UnormSrgb = 2, Depth32Float = 3, Bgra8UnormSrgb = 4, Depth24PlusStencil8 = 5 }
enum VertexFormat { Float32x2, Float32x3, Float32x4, Uint32 }
#[idl("GPUBlendFactor")]
enum BlendFactor {}
#[idl("GPUBlendOperation")]
enum BlendOperation {}
#[idl("GPUCompareFunction")]
enum CompareFunction {}
#[idl("GPUPrimitiveTopology")]
enum PrimitiveTopology {}
#[idl("GPUCullMode")]
enum CullMode {}
#[idl("GPUFrontFace")]
enum FrontFace {}
#[idl("GPUVertexStepMode")]
enum VertexStepMode {}
#[idl("GPUStencilOperation")]
enum StencilOperation {}
#[idl("GPUFilterMode")]
enum FilterMode {}
#[idl("GPUMipmapFilterMode")]
enum MipmapFilterMode {}
#[idl("GPUAddressMode")]
enum AddressMode {}
#[idl("GPUIndexFormat")]
enum IndexFormat {}
#[idl("GPUTextureViewDimension")]
enum TextureViewDimension {}
#[idl("GPUTextureAspect")]
enum TextureAspect {}
#[idl("GPUTextureDimension")]
enum TextureDimension {}
#[idl("GPUBufferUsage")]
mod BufferUsage {}
#[idl("GPUTextureUsage")]
mod TextureUsage {}
#[idl("GPUColorWrite")]
mod ColorWrite {}

// Dictionary-like values are plugin-owned Caribou objects. Bindgen imports
// fields, required markers, typedefs and inherited dictionary members.
#[idl("GPUBufferDescriptor")]
struct GpuBufferDescriptor {}
#[idl("GPUSamplerDescriptor")]
struct GpuSamplerDescriptor {}
#[idl("GPUTextureViewDescriptor")]
struct GpuTextureViewDescriptor {}
#[idl("GPUExtent3DDict")]
struct GpuExtent3D {}
#[idl("GPUTextureDescriptor")]
struct GpuTextureDescriptor {
    // WebIDL also permits a three-element sequence. A typed record keeps the
    // cross-language API explicit and avoids a dynamic union.
    size: GpuExtent3D,
}

trait GpuInstance {
    #[native(is_valid)]
    fn valid(this: &GpuInstance) -> bool;
    #[native(instance_create)]
    fn new() -> Box<GpuInstance>;
    #[native(instance_destroy)]
    fn destroy(this: &GpuInstance);
    #[native(adapter_open)]
    fn requestAdapter(this: &GpuInstance, power: Enum<Power>) -> Box<GpuAdapter>;
    #[native(surface_create)]
    fn surface(this: &GpuInstance, platform: i32, wa: i64, wb: i64, da: i64, db: i64) -> Box<GpuSurface>;
}

trait GpuAdapter {
    #[native(is_valid)]
    fn valid(this: &GpuAdapter) -> bool;
    #[native(adapter_name)]
    fn name(this: &GpuAdapter) -> Text;
    #[native(adapter_backend)]
    fn backend(this: &GpuAdapter) -> Enum<Backend>;
    #[native(adapter_limit)]
    fn limit(this: &GpuAdapter, which: Enum<Limit>) -> i64;
    #[native(adapter_destroy)]
    fn destroy(this: &GpuAdapter);
    #[native(device_open)]
    fn requestDevice(this: &GpuAdapter) -> Box<GpuDevice>;
    #[native(adapter_driver)]
    fn driver(this: &GpuAdapter) -> Text;
    #[native(adapter_driver_info)]
    fn driverInfo(this: &GpuAdapter) -> Text;
}

trait GpuDevice {
    #[native(is_valid)]
    fn valid(this: &GpuDevice) -> bool;
    #[native(device_take_error)]
    fn takeError(this: &GpuDevice) -> Text;
    #[native(device_queue)]
    fn queue(this: &GpuDevice) -> Box<GpuQueue>;
    #[native(device_poll)]
    fn poll(this: &GpuDevice);
    #[native(device_destroy)]
    fn destroy(this: &GpuDevice);
    #[native(buffer_create)]
    fn createBuffer(this: &GpuDevice, descriptor: &GpuBufferDescriptor) -> Box<GpuBuffer>;
    #[native(buffer_map_begin)]
    fn mapBuffer(this: &GpuDevice, buffer: &GpuBuffer, offset: i64, size: i64) -> Box<GpuRequest>;
    #[native(shader_create)]
    fn createShader(this: &GpuDevice, wgsl: Text) -> Box<GpuShader>;
    #[native(compute_pipeline_create)]
    fn computePipeline(this: &GpuDevice, shader: &GpuShader, entry: Text) -> Box<GpuPipeline>;
    #[native(bind_group_create)]
    fn bindGroup(this: &GpuDevice, pipeline: &GpuPipeline, group: i32, bindings: &GpuBindings) -> Box<GpuBindGroup>;
    #[native(encoder_create)]
    fn encoder(this: &GpuDevice) -> Box<GpuEncoder>;
    #[native(queue_work_done)]
    fn queueWorkDone(this: &GpuDevice, queue: &GpuQueue) -> Box<GpuRequest>;
    #[native(texture_create)]
    fn texture(this: &GpuDevice, descriptor: &GpuTextureDescriptor) -> Box<GpuTexture>;
    #[native(pipeline_begin)]
    fn pipeline(this: &GpuDevice) -> Box<GpuPipelineBuilder>;
    #[native(sampler_create)]
    fn sampler(this: &GpuDevice, descriptor: &GpuSamplerDescriptor) -> Box<GpuSampler>;
    #[native(surface_configure)]
    fn configureSurface(this: &GpuDevice, surface: &GpuSurface, width: i32, height: i32, format: Enum<TextureFormat>);
}

trait GpuQueue {
    #[native(is_valid)]
    fn valid(this: &GpuQueue) -> bool;
    #[native(queue_write_buffer)]
    fn writeBuffer(this: &GpuQueue, buffer: &GpuBuffer, offset: i64, data: Buffer, len: i32);
    #[native(queue_write_texture)]
    fn writeTexture(this: &GpuQueue, texture: &GpuTexture, data: Buffer, width: i32, height: i32, bytes_per_row: i32);
    #[native(surface_present)]
    fn presentSurface(this: &GpuQueue, surface: &GpuSurface);
}

trait GpuBuffer {
    #[native(is_valid)]
    fn valid(this: &GpuBuffer) -> bool;
    #[native(buffer_copy_out)]
    fn copyOut(this: &GpuBuffer, offset: i64, out: Buffer, len: i32) -> bool;
    #[native(buffer_unmap)]
    fn unmap(this: &GpuBuffer);
    #[native(buffer_destroy)]
    fn destroy(this: &GpuBuffer);
}

trait GpuShader {
    #[native(is_valid)]
    fn valid(this: &GpuShader) -> bool;
    #[native(shader_destroy)]
    fn destroy(this: &GpuShader);
    #[native(shader_messages)]
    fn messages(this: &GpuShader) -> Text;
}

trait GpuPipeline {
    #[native(is_valid)]
    fn valid(this: &GpuPipeline) -> bool;
    #[native(pipeline_release)]
    fn destroy(this: &GpuPipeline);
}

trait GpuBindGroup {
    #[native(is_valid)]
    fn valid(this: &GpuBindGroup) -> bool;
    #[native(bind_group_destroy)]
    fn destroy(this: &GpuBindGroup);
}

trait GpuEncoder {
    #[native(encoder_destroy)]
    fn destroy(this: &GpuEncoder);
    #[native(is_valid)]
    fn valid(this: &GpuEncoder) -> bool;
    #[native(encoder_compute)]
    fn compute(this: &GpuEncoder, pipeline: &GpuPipeline, bindgroup: &GpuBindGroup, x: i32, y: i32, z: i32);
    #[native(encoder_copy_buffer)]
    fn copyBuffer(this: &GpuEncoder, src: &GpuBuffer, src_offset: i64, dst: &GpuBuffer, dst_offset: i64, size: i64);
    #[native(encoder_submit)]
    fn submit(this: &GpuEncoder, queue: &GpuQueue);
    #[native(pass_reset)]
    fn passReset(this: &GpuEncoder);
    #[native(pass_colour)]
    fn passColour(this: &GpuEncoder, view: &GpuTextureView, r: f64, g: f64, b: f64, a: f64);
    #[native(pass_depth)]
    fn passDepth(this: &GpuEncoder, view: &GpuTextureView, clear: f64, stencil_clear: i32);
    #[native(pass_begin)]
    fn passBegin(this: &GpuEncoder);
    #[native(render_set_pipeline)]
    fn renderSetPipeline(this: &GpuEncoder, pipeline: &GpuPipeline);
    #[native(render_set_vertex_buffer)]
    fn renderSetVertexBuffer(this: &GpuEncoder, slot: i32, buffer: &GpuBuffer);
    #[native(render_set_viewport)]
    fn renderSetViewport(this: &GpuEncoder, x: f64, y: f64, width: f64, height: f64, min_depth: f64, max_depth: f64);
    #[native(render_set_scissor_rect)]
    fn renderSetScissorRect(this: &GpuEncoder, x: i32, y: i32, width: i32, height: i32);
    #[native(render_draw)]
    fn renderDraw(this: &GpuEncoder, vertices: i32, instances: i32);
    #[native(encoder_render_end)]
    fn renderEnd(this: &GpuEncoder);
    #[native(encoder_copy_buffer_to_texture)]
    fn copyBufferToTexture(this: &GpuEncoder, buffer: &GpuBuffer, bytes_per_row: i32, texture: &GpuTexture, width: i32, height: i32);
    #[native(encoder_copy_texture_to_texture)]
    fn copyTextureToTexture(this: &GpuEncoder, src: &GpuTexture, dst: &GpuTexture, width: i32, height: i32);
    #[native(encoder_clear_buffer)]
    fn clearBuffer(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, size: i64);
    #[native(encoder_copy_texture_to_buffer)]
    fn copyTextureToBuffer(this: &GpuEncoder, texture: &GpuTexture, buffer: &GpuBuffer, width: i32, height: i32, bytes_per_row: i32);
    #[native(render_set_bind_group)]
    fn renderSetBindGroup(this: &GpuEncoder, group: i32, bindgroup: &GpuBindGroup);
    #[native(render_set_index_buffer)]
    fn renderSetIndexBuffer(this: &GpuEncoder, buffer: &GpuBuffer, format: Enum<IndexFormat>);
    #[native(render_draw_indexed)]
    fn renderDrawIndexed(this: &GpuEncoder, indices: i32, instances: i32);
    #[native(render_set_blend_constant)]
    fn renderSetBlendConstant(this: &GpuEncoder, r: f64, g: f64, b: f64, a: f64);
    #[native(render_set_stencil_reference)]
    fn renderSetStencilReference(this: &GpuEncoder, reference: i32);
    #[native(encoder_compute_indirect)]
    fn computeIndirect(this: &GpuEncoder, pipeline: &GpuPipeline, bindgroup: &GpuBindGroup, buffer: &GpuBuffer, offset: i64);
    #[native(render_draw_indirect)]
    fn renderDrawIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64);
    #[native(render_draw_indexed_indirect)]
    fn renderDrawIndexedIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64);
    #[native(encoder_push_debug_group)]
    fn pushDebugGroup(this: &GpuEncoder, label: Text);
    #[native(encoder_pop_debug_group)]
    fn popDebugGroup(this: &GpuEncoder);
    #[native(encoder_insert_debug_marker)]
    fn insertDebugMarker(this: &GpuEncoder, label: Text);
}

trait GpuTexture {
    #[native(is_valid)]
    fn valid(this: &GpuTexture) -> bool;
    #[native(texture_view)]
    fn createView(this: &GpuTexture, descriptor: &GpuTextureViewDescriptor) -> Box<GpuTextureView>;
    #[native(texture_destroy)]
    fn destroy(this: &GpuTexture);
}

trait GpuTextureView {
    #[native(is_valid)]
    fn valid(this: &GpuTextureView) -> bool;
    #[native(view_destroy)]
    fn destroy(this: &GpuTextureView);
}

trait GpuSampler {
    #[native(is_valid)]
    fn valid(this: &GpuSampler) -> bool;
    #[native(sampler_destroy)]
    fn destroy(this: &GpuSampler);
}

trait GpuPipelineBuilder {
    #[native(builder_destroy)]
    fn destroy(this: &GpuPipelineBuilder);
    #[native(is_valid)]
    fn valid(this: &GpuPipelineBuilder) -> bool;
    #[native(pipeline_shader)]
    fn shader(this: &GpuPipelineBuilder, shader: &GpuShader, vs: Text, fs: Text);
    #[native(pipeline_vertex_buffer)]
    fn vertexBuffer(this: &GpuPipelineBuilder, stride: i64, step: Enum<VertexStepMode>);
    #[native(pipeline_attribute_packed)]
    fn attributePacked(this: &GpuPipelineBuilder, format: Enum<VertexFormat>);
    #[native(pipeline_attribute)]
    fn attribute(this: &GpuPipelineBuilder, format: Enum<VertexFormat>, offset: i64, location: i32);
    #[native(pipeline_target)]
    fn target(this: &GpuPipelineBuilder, format: Enum<TextureFormat>, write_mask: i32);
    #[native(pipeline_blend)]
    fn blend(this: &GpuPipelineBuilder, src: Enum<BlendFactor>, dst: Enum<BlendFactor>, op: Enum<BlendOperation>, src_alpha: Enum<BlendFactor>, dst_alpha: Enum<BlendFactor>, op_alpha: Enum<BlendOperation>);
    #[native(pipeline_stencil)]
    fn stencil(this: &GpuPipelineBuilder, compare: Enum<CompareFunction>, fail: Enum<StencilOperation>, depth_fail: Enum<StencilOperation>, pass_op: Enum<StencilOperation>, read_mask: i32, write_mask: i32);
    #[native(pipeline_depth)]
    fn depth(this: &GpuPipelineBuilder, format: Enum<TextureFormat>, write: bool, compare: Enum<CompareFunction>);
    #[native(pipeline_primitive)]
    fn primitive(this: &GpuPipelineBuilder, topology: Enum<PrimitiveTopology>, cull: Enum<CullMode>, front: Enum<FrontFace>);
    #[native(render_pipeline_build)]
    fn build(this: &GpuPipelineBuilder) -> Box<GpuPipeline>;
}

trait GpuSurface {
    #[native(is_valid)]
    fn valid(this: &GpuSurface) -> bool;
    #[native(surface_preferred_format)]
    fn preferredFormat(this: &GpuSurface, adapter: &GpuAdapter) -> Enum<TextureFormat>;
    #[native(surface_acquire)]
    fn acquire(this: &GpuSurface) -> Box<GpuTextureView>;
    #[native(surface_destroy)]
    fn destroy(this: &GpuSurface);
}

trait GpuRequest {
    #[native(request_ready)]
    fn ready(this: &GpuRequest) -> bool;
    #[native(request_result)]
    fn result(this: &GpuRequest) -> i32;
    #[native(request_discard)]
    fn destroy(this: &GpuRequest);
}

trait GpuBindings {
    #[native(bindings_create)]
    fn new() -> Box<GpuBindings>;
    #[native(is_valid)]
    fn valid(this: &GpuBindings) -> bool;
    #[native(bindings_buffer)]
    fn buffer(this: &GpuBindings, buffer: &GpuBuffer);
    #[native(bindings_view)]
    fn texture(this: &GpuBindings, view: &GpuTextureView);
    #[native(bindings_sampler)]
    fn sampler(this: &GpuBindings, sampler: &GpuSampler);
    #[native(bindings_destroy)]
    fn destroy(this: &GpuBindings);
}
