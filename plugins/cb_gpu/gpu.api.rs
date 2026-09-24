// This declaration generates object wrappers and the entire plugin! table.
// Native functions live in src/backend.rs. Enum ordinals are backend values,
// not the layout of Caribou enums. WebIDL is vendored for reproducible builds.
enum Power { None = -1, LowPower = 0, HighPerformance = 1 }
enum Backend { Noop = 0, Vulkan = 1, Metal = 2, Dx12 = 3, Gl = 4, BrowserWebGpu = 5 }
enum DeviceType { Other, IntegratedGpu, DiscreteGpu, VirtualGpu, Cpu }
enum MemoryHints { Performance, MemoryUsage }
#[idl("GPUFeatureName")]
enum Feature {}
#[idl("GPUSupportedLimits")]
enum Limit {}
#[idl("GPUTextureFormat")]
enum TextureFormat {
    // wgpu's own: TEXTURE_INT64_ATOMIC, TEXTURE_FORMAT_NV12, TEXTURE_FORMAT_P010.
    #[extension] R64uint,
    #[extension] Nv12,
    #[extension] P010,
}
#[idl("GPUVertexFormat")]
enum VertexFormat {
    // wgpu's own, with VERTEX_ATTRIBUTE_64BIT.
    #[extension] Float64,
    #[extension] Float64x2,
    #[extension] Float64x3,
    #[extension] Float64x4,
}
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
enum AddressMode {
    // wgpu's own, with ADDRESS_MODE_CLAMP_TO_BORDER; see borderColor.
    #[extension] ClampToBorder,
}
#[idl("GPUIndexFormat")]
enum IndexFormat {}
#[idl("GPUTextureViewDimension")]
enum TextureViewDimension {}
#[idl("GPUTextureAspect")]
enum TextureAspect {}
#[idl("GPUTextureDimension")]
enum TextureDimension {}
#[idl("GPUBufferBindingType")]
enum BufferBindingType {}
#[idl("GPUSamplerBindingType")]
enum SamplerBindingType {}
#[idl("GPUTextureSampleType")]
enum TextureSampleType {}
#[idl("GPUStorageTextureAccess")]
enum StorageTextureAccess {
    // wgpu's own, with TEXTURE_ATOMIC.
    #[extension] Atomic,
}
// wgpu's DontCare leaves an attachment undefined until it is written. A
// device takes it only with dontCareLoads on its descriptor.
#[idl("GPULoadOp")]
enum LoadOp {
    #[extension]
    DontCare,
}
#[idl("GPUStoreOp")]
enum StoreOp {}
#[idl("GPUQueryType")]
enum QueryType {
    // wgpu's own, with PIPELINE_STATISTICS_QUERY; see pipelineStatistics.
    #[extension] PipelineStatistics,
}
#[idl("GPUErrorFilter")]
enum ErrorFilter {}
#[idl("GPUDeviceLostReason")]
enum DeviceLostReason {}
#[idl("GPUCompilationMessageType")]
enum CompilationMessageType {}
// wgpu's own render and surface choices, beyond WebGPU's.
enum PolygonMode { Fill, Line, Point }
enum BorderColor { TransparentBlack, OpaqueBlack, OpaqueWhite, Zero }
enum PresentMode { AutoVsync, AutoNoVsync, Fifo, FifoRelaxed, Immediate, Mailbox }
enum AlphaMode { Auto, Opaque, PreMultiplied, PostMultiplied, Inherit }
enum ColorSpace { Auto, Srgb, ExtendedSrgbLinear, DisplayP3 }
// wgpu's limits beyond WebGPU's (see Limit): binding arrays, mesh shaders,
// ray tracing and multiview.
enum NativeLimit {
    MaxBindingArrayElementsPerShaderStage,
    MaxBindingArrayAccelerationStructureElementsPerShaderStage,
    MaxBindingArraySamplerElementsPerShaderStage,
    MaxNonSamplerBindings,
    MaxTaskWorkgroupTotalCount,
    MaxTaskWorkgroupsPerDimension,
    MaxMeshWorkgroupTotalCount,
    MaxMeshWorkgroupsPerDimension,
    MaxTaskInvocationsPerWorkgroup,
    MaxTaskInvocationsPerDimension,
    MaxMeshInvocationsPerWorkgroup,
    MaxMeshInvocationsPerDimension,
    MaxTaskPayloadSize,
    MaxMeshOutputVertices,
    MaxMeshOutputPrimitives,
    MaxMeshOutputLayers,
    MaxMeshMultiviewViewCount,
    MaxBlasPrimitiveCount,
    MaxBlasGeometryCount,
    MaxTlasInstanceCount,
    MaxAccelerationStructuresPerShaderStage,
    MaxBuffersAndAccelerationStructuresPerShaderStage,
    MaxMultiviewViewCount,
    MaxRayDispatchCount,
    MaxRayRecursionDepth,
}
#[idl("GPUMapMode")]
mod MapMode {}
// wgpu's own bits follow the IDL's.
#[idl("GPUBufferUsage")]
mod BufferUsage {
    const BLAS_INPUT: i32 = 1024;
    const TLAS_INPUT: i32 = 2048;
}
#[idl("GPUShaderStage")]
mod ShaderStage {
    const TASK: i32 = 8;
    const MESH: i32 = 16;
}
#[idl("GPUTextureUsage")]
mod TextureUsage {
    const STORAGE_ATOMIC: i32 = 65536;
}
#[idl("GPUColorWrite")]
mod ColorWrite {}
// What a pipeline statistics query counts.
mod PipelineStatistic {
    const VERTEX_SHADER_INVOCATIONS: i32 = 1;
    const CLIPPER_INVOCATIONS: i32 = 2;
    const CLIPPER_PRIMITIVES_OUT: i32 = 4;
    const FRAGMENT_SHADER_INVOCATIONS: i32 = 8;
    const COMPUTE_SHADER_INVOCATIONS: i32 = 16;
}
// What a texture format supports on an adapter, beyond its usages.
mod TextureFormatFeature {
    const FILTERABLE: i32 = 1;
    const MULTISAMPLE_X2: i32 = 2;
    const MULTISAMPLE_X4: i32 = 4;
    const MULTISAMPLE_X8: i32 = 8;
    const MULTISAMPLE_X16: i32 = 16;
    const MULTISAMPLE_RESOLVE: i32 = 32;
    const STORAGE_READ_ONLY: i32 = 64;
    const STORAGE_WRITE_ONLY: i32 = 128;
    const STORAGE_READ_WRITE: i32 = 256;
    const STORAGE_ATOMIC: i32 = 512;
    const BLENDABLE: i32 = 1024;
}

// Dictionary-like values are plugin-owned Caribou objects. Bindgen imports
// fields, required markers, typedefs and inherited dictionary members.
#[idl("GPUBufferDescriptor")]
struct GpuBufferDescriptor {}
#[idl("GPUSamplerDescriptor")]
struct GpuSamplerDescriptor {
    // wgpu's own, for the ClampToBorder address mode.
    #[extension] borderColor: Option<Enum<BorderColor>>,
}
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
// wgpu's backends and instance flags, bit for bit.
mod Backends {
    const VULKAN: i32 = 2;
    const METAL: i32 = 4;
    const DX12: i32 = 8;
    const GL: i32 = 16;
    const BROWSER_WEBGPU: i32 = 32;
}
mod InstanceFlag {
    const DEBUG: i32 = 1;
    const VALIDATION: i32 = 2;
    const DISCARD_HAL_LABELS: i32 = 4;
    const ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER: i32 = 8;
    const GPU_BASED_VALIDATION: i32 = 16;
    const VALIDATION_INDIRECT_CALL: i32 = 32;
    const AUTOMATIC_TIMESTAMP_NORMALIZATION: i32 = 64;
    const STRICT_WEBGPU_COMPLIANCE: i32 = 128;
}
// Unset members are wgpu's defaults for this build.
struct GpuInstanceDescriptor {
    backends: Option<i32>,
    flags: Option<i32>,
}
// WebGPU's adapter options, with wgpu's compatible surface. wgpu has no
// feature levels and no XR, so those two members are left out.
struct GpuRequestAdapterOptions {
    powerPreference: Option<Enum<Power>>,
    forceFallbackAdapter: Option<bool>,
    compatibleSurface: Option<GpuSurface>,
}
struct GpuDeviceDescriptor {
    requiredFeatures: Vec<Enum<Feature>>,
    requiredLimits: Map<Enum<Limit>, i64>,
    // wgpu's own features and limits, beyond WebGPU's.
    requiredNativeFeatures: Vec<Enum<NativeFeature>>,
    requiredNativeLimits: Map<Enum<NativeLimit>, i64>,
    memoryHints: Option<Enum<MemoryHints>>,
    // Accepts wgpu's terms for its EXPERIMENTAL_* features, which may still
    // have bugs that are undefined behaviour. Requesting one needs this.
    experimentalFeatures: Option<bool>,
    // Accepts DontCare loads: an attachment loaded that way is undefined,
    // and reading it before every pixel is written is undefined behaviour.
    dontCareLoads: Option<bool>,
    // Accepts shaders wgpu does not check: passthrough shaders, and shader
    // modules with runtime checks turned off. The program promises that the
    // code is valid for the backend, stays in bounds and terminates.
    trustedShaders: Option<bool>,
}

// A WGSL module with wgpu's runtime checks, each on unless turned off.
// Turning one off needs trustedShaders on the device.
struct GpuShaderModuleDescriptor {
    code: Text,
    label: Option<Text>,
    boundsChecks: Option<bool>,
    forceLoopBounding: Option<bool>,
    rayQueryInitializationTracking: Option<bool>,
    taskShaderDispatchTracking: Option<bool>,
    meshShaderPrimitiveIndicesClamp: Option<bool>,
    intDivChecks: Option<bool>,
}
// A shader handed to the backend as it is, with PASSTHROUGH_SHADERS and
// trustedShaders. The backend in use takes its own source: SPIR-V words on
// Vulkan, DXIL or HLSL on DX12, a metallib or MSL on Metal, WGSL in the
// browser. Metal reads each compute entry point's workgroup size from here.
struct GpuPassthroughEntryPoint {
    name: Text,
    workgroupX: Option<i32>,
    workgroupY: Option<i32>,
    workgroupZ: Option<i32>,
}
struct GpuPassthroughShaderDescriptor {
    label: Option<Text>,
    entryPoints: Vec<GpuPassthroughEntryPoint>,
    spirv: Option<Buffer>,
    dxil: Option<Buffer>,
    hlsl: Option<Text>,
    metallib: Option<Buffer>,
    msl: Option<Text>,
    wgsl: Option<Text>,
}

// Explicit layouts: what a pipeline's bind groups hold, declared ahead of
// the shaders instead of inferred from one pipeline.
#[idl("GPUBufferBindingLayout")]
struct GpuBufferBindingLayout {}
#[idl("GPUSamplerBindingLayout")]
struct GpuSamplerBindingLayout {}
#[idl("GPUTextureBindingLayout")]
struct GpuTextureBindingLayout {}
#[idl("GPUStorageTextureBindingLayout")]
struct GpuStorageTextureBindingLayout {}
#[idl("GPUExternalTextureBindingLayout")]
struct GpuExternalTextureBindingLayout {}
// wgpu's own binding kinds: a top-level acceleration structure, and a
// binding array of `count` elements of any kind.
struct GpuAccelerationStructureBindingLayout {
    vertexReturn: Option<bool>,
}
#[idl("GPUBindGroupLayoutEntry")]
struct GpuBindGroupLayoutEntry {
    #[extension]
    count: Option<i32>,
    #[extension]
    accelerationStructure: Option<GpuAccelerationStructureBindingLayout>,
}
#[idl("GPUBindGroupLayoutDescriptor")]
struct GpuBindGroupLayoutDescriptor {}
#[idl("GPUPipelineLayoutDescriptor")]
struct GpuPipelineLayoutDescriptor {}

// A bind group entry is one of these; each alternative is its own setter,
// `resourceBuffer(buffer)`, `resourceBufferBinding(range)` and so on. The
// arrays fill a layout entry with a count.
#[idl("GPUBufferBinding")]
struct GpuBufferBinding {}
struct GpuBufferArray {
    buffers: Vec<GpuBufferBinding>,
}
struct GpuSamplerArray {
    samplers: Vec<GpuSampler>,
}
struct GpuTextureViewArray {
    views: Vec<GpuTextureView>,
}
struct GpuTlasArray {
    tlases: Vec<GpuTlas>,
}
#[idl("GPUBindingResource")]
enum BindingResource {
    Sampler(GpuSampler),
    Texture(GpuTexture),
    TextureView(GpuTextureView),
    Buffer(GpuBuffer),
    BufferBinding(GpuBufferBinding),
    ExternalTexture(GpuExternalTexture),
    #[extension]
    BufferArray(GpuBufferArray),
    #[extension]
    SamplerArray(GpuSamplerArray),
    #[extension]
    TextureViewArray(GpuTextureViewArray),
    #[extension]
    AccelerationStructure(GpuTlas),
    #[extension]
    AccelerationStructureArray(GpuTlasArray),
}
#[idl("GPUBindGroupEntry")]
struct GpuBindGroupEntry {}
#[idl("GPUBindGroupDescriptor")]
struct GpuBindGroupDescriptor {}

#[idl("GPUProgrammableStage")]
struct GpuProgrammableStage {}
#[idl("GPUComputePipelineDescriptor")]
struct GpuComputePipelineDescriptor {
    // WebIDL's layout is a pipeline layout or "auto". Unset is "auto".
    layout: Option<GpuPipelineLayout>,
}

// Render pipelines from WebGPU's descriptors.
#[idl("GPUColorDict")]
struct GpuColor {}
#[idl("GPUBlendComponent")]
struct GpuBlendComponent {}
#[idl("GPUBlendState")]
struct GpuBlendState {}
#[idl("GPUColorTargetState")]
struct GpuColorTargetState {}
#[idl("GPUVertexAttribute")]
struct GpuVertexAttribute {}
#[idl("GPUVertexBufferLayout")]
struct GpuVertexBufferLayout {}
#[idl("GPUVertexState")]
struct GpuVertexState {}
#[idl("GPUFragmentState")]
struct GpuFragmentState {}
#[idl("GPUPrimitiveState")]
struct GpuPrimitiveState {
    // wgpu's own: POLYGON_MODE_LINE/POINT and CONSERVATIVE_RASTERIZATION.
    #[extension] polygonMode: Option<Enum<PolygonMode>>,
    #[extension] conservative: Option<bool>,
}
#[idl("GPUStencilFaceState")]
struct GpuStencilFaceState {}
#[idl("GPUDepthStencilState")]
struct GpuDepthStencilState {}
#[idl("GPUMultisampleState")]
struct GpuMultisampleState {}
#[idl("GPURenderPipelineDescriptor")]
struct GpuRenderPipelineDescriptor {
    // Unset is WebGPU's "auto".
    layout: Option<GpuPipelineLayout>,
    // wgpu's own, with MULTIVIEW: the views the pipeline renders.
    #[extension] multiviewMask: Option<i32>,
}

// Render and compute passes from WebGPU's descriptors. An attachment's view
// is a texture (its default view) or a texture view.
enum AttachmentView {
    Texture(GpuTexture),
    TextureView(GpuTextureView),
}
#[idl("GPURenderPassColorAttachment")]
struct GpuRenderPassColorAttachment {
    view: AttachmentView,
    resolveTarget: Option<AttachmentView>,
    // WebIDL also permits a four-element sequence.
    clearValue: Option<GpuColor>,
}
#[idl("GPURenderPassDepthStencilAttachment")]
struct GpuRenderPassDepthStencilAttachment {
    view: AttachmentView,
}
#[idl("GPURenderPassTimestampWrites")]
struct GpuRenderPassTimestampWrites {}
#[idl("GPURenderPassDescriptor")]
struct GpuRenderPassDescriptor {}
#[idl("GPUComputePassTimestampWrites")]
struct GpuComputePassTimestampWrites {}
#[idl("GPUComputePassDescriptor")]
struct GpuComputePassDescriptor {}

// Copies with their full source and destination descriptions.
#[idl("GPUOrigin3DDict")]
struct GpuOrigin3D {}
#[idl("GPUTexelCopyBufferLayout")]
struct GpuTexelCopyBufferLayout {}
#[idl("GPUTexelCopyBufferInfo")]
struct GpuTexelCopyBufferInfo {}
#[idl("GPUTexelCopyTextureInfo")]
struct GpuTexelCopyTextureInfo {
    // WebIDL also permits a three-element sequence.
    origin: Option<GpuOrigin3D>,
}

#[idl("GPUQuerySetDescriptor")]
struct GpuQuerySetDescriptor {
    // wgpu's own: what a PipelineStatistics set counts (PipelineStatistic).
    #[extension] pipelineStatistics: Option<i32>,
}
#[idl("GPURenderBundleEncoderDescriptor")]
struct GpuRenderBundleEncoderDescriptor {}

// wgpu's mesh pipelines, with EXPERIMENTAL_MESH_SHADER: an optional task
// stage and a mesh stage in place of vertex input. `multiview` is the
// attachments' layer count.
struct GpuMeshPipelineDescriptor {
    mesh: GpuProgrammableStage,
    label: Option<Text>,
    layout: Option<GpuPipelineLayout>,
    task: Option<GpuProgrammableStage>,
    primitive: Option<GpuPrimitiveState>,
    depthStencil: Option<GpuDepthStencilState>,
    multisample: Option<GpuMultisampleState>,
    fragment: Option<GpuFragmentState>,
    multiview: Option<i32>,
}

// wgpu's external textures: one to three planes, and how sampling turns
// them into RGBA. WebIDL's descriptor takes a browser video frame instead.
// Unset matrices and transforms are the identity; a width and height of 0
// are the first plane's.
enum ExternalTextureFormat { Rgba, Nv12, Yu12 }
struct GpuExternalTextureTransferFunction {
    a: f32,
    b: f32,
    g: f32,
    k: f32,
}
struct GpuExternalTextureDescriptor {
    format: Enum<ExternalTextureFormat>,
    planes: Vec<GpuTextureView>,
    label: Option<Text>,
    width: Option<i32>,
    height: Option<i32>,
    // Column-major 4x4, 3x3, 3x2 and 3x2.
    yuvConversionMatrix: Vec<f32>,
    gamutConversionMatrix: Vec<f32>,
    sampleTransform: Vec<f32>,
    loadTransform: Vec<f32>,
    srcTransferFunction: Option<GpuExternalTextureTransferFunction>,
    dstTransferFunction: Option<GpuExternalTextureTransferFunction>,
}

// Ray tracing, wgpu's own with EXPERIMENTAL_RAY_QUERY. A bottom-level
// structure (BLAS) holds triangles or boxes; a top-level one (TLAS) holds
// transformed instances of BLASes and binds to shaders' ray queries.
mod AccelerationStructureFlag {
    const ALLOW_UPDATE: i32 = 1;
    const ALLOW_COMPACTION: i32 = 2;
    const PREFER_FAST_TRACE: i32 = 4;
    const PREFER_FAST_BUILD: i32 = 8;
    const LOW_MEMORY: i32 = 16;
    const USE_TRANSFORM: i32 = 32;
    const ALLOW_RAY_HIT_VERTEX_RETURN: i32 = 64;
}
mod AccelerationStructureGeometryFlag {
    const OPAQUE: i32 = 1;
    const NO_DUPLICATE_ANY_HIT_INVOCATION: i32 = 2;
}
enum AccelerationStructureUpdateMode { Build, PreferUpdate }
struct GpuBlasTriangleGeometrySize {
    vertexFormat: Enum<VertexFormat>,
    vertexCount: i32,
    indexFormat: Option<Enum<IndexFormat>>,
    indexCount: Option<i32>,
    flags: Option<i32>,
}
struct GpuBlasAabbGeometrySize {
    primitiveCount: i32,
    flags: Option<i32>,
}
// Triangles or boxes, not both.
struct GpuBlasDescriptor {
    label: Option<Text>,
    flags: Option<i32>,
    updateMode: Option<Enum<AccelerationStructureUpdateMode>>,
    triangles: Vec<GpuBlasTriangleGeometrySize>,
    aabbs: Vec<GpuBlasAabbGeometrySize>,
}
struct GpuTlasDescriptor {
    maxInstances: i32,
    label: Option<Text>,
    flags: Option<i32>,
    updateMode: Option<Enum<AccelerationStructureUpdateMode>>,
}
// `transform` is a row-major 3x4 matrix, the identity when unset.
// `customData` is 24 bits and `mask` 8, all set when unset.
struct GpuTlasInstance {
    blas: GpuBlas,
    transform: Vec<f32>,
    customData: Option<i32>,
    mask: Option<i32>,
}
// Each geometry's size must be the one its BLAS was created with.
struct GpuBlasTriangleGeometry {
    size: GpuBlasTriangleGeometrySize,
    vertexBuffer: GpuBuffer,
    vertexStride: i64,
    firstVertex: Option<i32>,
    indexBuffer: Option<GpuBuffer>,
    firstIndex: Option<i32>,
    transformBuffer: Option<GpuBuffer>,
    transformBufferOffset: Option<i64>,
}
struct GpuBlasAabbGeometry {
    size: GpuBlasAabbGeometrySize,
    aabbBuffer: GpuBuffer,
    stride: i64,
    primitiveOffset: Option<i32>,
}
struct GpuBlasBuildEntry {
    blas: GpuBlas,
    triangles: Vec<GpuBlasTriangleGeometry>,
    aabbs: Vec<GpuBlasAabbGeometry>,
}
struct GpuAccelerationStructureBuild {
    blases: Vec<GpuBlasBuildEntry>,
    tlases: Vec<GpuTlas>,
}

// A native surface's configuration: WebGPU's canvas configuration, sized
// and with wgpu's present modes, latency and color spaces.
struct GpuSurfaceConfiguration {
    format: Enum<TextureFormat>,
    width: i32,
    height: i32,
    usage: Option<i32>,
    viewFormats: Vec<Enum<TextureFormat>>,
    alphaMode: Option<Enum<AlphaMode>>,
    presentMode: Option<Enum<PresentMode>>,
    colorSpace: Option<Enum<ColorSpace>>,
    desiredMaximumFrameLatency: Option<i32>,
}

#[idl("GPU")]
trait GpuInstance {
    #[native(is_valid)]
    fn valid(this: &GpuInstance) -> bool;
    #[native(instance_create)]
    fn new() -> Box<GpuInstance>;
    #[native(instance_destroy)]
    fn destroy(this: &GpuInstance);
    #[native(adapter_open)]
    #[idl("GPU.requestAdapter")]
    fn requestAdapter(this: &GpuInstance, power: Enum<Power>) -> Future<GpuAdapter>;
    #[native(instance_create_with)]
    fn createWith(descriptor: &GpuInstanceDescriptor) -> Box<GpuInstance>;
    #[native(adapter_request_with)]
    #[idl("GPU.requestAdapter")]
    fn requestAdapterWith(this: &GpuInstance, options: &GpuRequestAdapterOptions) -> Future<GpuAdapter>;
    #[native(surface_create)]
    fn surface(this: &GpuInstance, platform: i32, wa: i64, wb: i64, da: i64, db: i64) -> Box<GpuSurface>;
}

#[idl("GPUAdapter")]
trait GpuAdapter {
    #[native(is_valid)]
    fn valid(this: &GpuAdapter) -> bool;
    #[native(adapter_name)]
    fn name(this: &GpuAdapter) -> Text;
    #[native(adapter_backend)]
    fn backend(this: &GpuAdapter) -> Enum<Backend>;
    #[native(adapter_limit)]
    fn limit(this: &GpuAdapter, which: Enum<Limit>) -> i64;
    #[native(adapter_feature)]
    fn supports(this: &GpuAdapter, feature: Enum<Feature>) -> bool;
    #[native(adapter_destroy)]
    fn destroy(this: &GpuAdapter);
    #[native(device_open)]
    #[idl("GPUAdapter.requestDevice")]
    fn requestDevice(this: &GpuAdapter) -> Future<GpuDevice>;
    #[native(device_open_with)]
    #[idl("GPUAdapter.requestDevice")]
    fn requestDeviceWith(this: &GpuAdapter, descriptor: &GpuDeviceDescriptor) -> Future<GpuDevice>;
    #[native(adapter_driver)]
    fn driver(this: &GpuAdapter) -> Text;
    #[native(adapter_driver_info)]
    fn driverInfo(this: &GpuAdapter) -> Text;
    #[native(adapter_native_feature)]
    fn supportsNative(this: &GpuAdapter, feature: Enum<NativeFeature>) -> bool;
    #[native(adapter_native_limit)]
    fn nativeLimit(this: &GpuAdapter, which: Enum<NativeLimit>) -> i64;
    // TextureUsage bits the format allows on this adapter.
    #[native(adapter_format_usages)]
    fn textureFormatUsages(this: &GpuAdapter, format: Enum<TextureFormat>) -> i32;
    // TextureFormatFeature bits: filtering, multisampling, storage access.
    #[native(adapter_format_features)]
    fn textureFormatFeatures(this: &GpuAdapter, format: Enum<TextureFormat>) -> i32;
    // PCI ids where the backend has them, 0 where it does not.
    #[native(adapter_vendor_id)]
    fn vendorId(this: &GpuAdapter) -> i64;
    #[native(adapter_device_id)]
    fn deviceId(this: &GpuAdapter) -> i64;
    #[native(adapter_device_type)]
    fn deviceType(this: &GpuAdapter) -> Enum<DeviceType>;
    #[native(adapter_pci_bus_id)]
    fn pciBusId(this: &GpuAdapter) -> Text;
    #[native(adapter_subgroup_min_size)]
    fn subgroupMinSize(this: &GpuAdapter) -> i32;
    #[native(adapter_subgroup_max_size)]
    fn subgroupMaxSize(this: &GpuAdapter) -> i32;
}

#[idl("GPUDevice")]
trait GpuDevice {
    #[native(is_valid)]
    fn valid(this: &GpuDevice) -> bool;
    #[native(device_take_error)]
    fn takeError(this: &GpuDevice) -> Text;
    #[native(device_queue)]
    fn queue(this: &GpuDevice) -> Box<GpuQueue>;
    #[native(device_poll)]
    fn poll(this: &GpuDevice);
    #[native(device_limit)]
    fn limit(this: &GpuDevice, which: Enum<Limit>) -> i64;
    #[native(device_feature)]
    fn supports(this: &GpuDevice, feature: Enum<Feature>) -> bool;
    #[native(device_destroy)]
    fn destroy(this: &GpuDevice);
    #[native(buffer_create)]
    fn createBuffer(this: &GpuDevice, descriptor: &GpuBufferDescriptor) -> Box<GpuBuffer>;
    #[native(buffer_map_begin)]
    #[idl("GPUBuffer.mapAsync")]
    fn mapBuffer(this: &GpuDevice, buffer: &GpuBuffer, offset: i64, size: i64) -> Future<()>;
    // `mode` is MapMode.READ or MapMode.WRITE.
    #[native(buffer_map_with)]
    #[idl("GPUBuffer.mapAsync")]
    fn mapBufferWith(this: &GpuDevice, buffer: &GpuBuffer, mode: i32, offset: i64, size: i64) -> Future<()>;
    #[native(shader_create)]
    fn createShader(this: &GpuDevice, wgsl: Text) -> Box<GpuShader>;
    #[native(shader_create_with)]
    fn createShaderModule(this: &GpuDevice, descriptor: &GpuShaderModuleDescriptor) -> Box<GpuShader>;
    #[native(shader_create_passthrough)]
    fn createShaderPassthrough(this: &GpuDevice, descriptor: &GpuPassthroughShaderDescriptor) -> Box<GpuShader>;
    #[native(compute_pipeline_create)]
    fn computePipeline(this: &GpuDevice, shader: &GpuShader, entry: Text) -> Box<GpuPipeline>;
    #[native(bind_group_create)]
    fn bindGroup(this: &GpuDevice, pipeline: &GpuPipeline, group: i32, bindings: &GpuBindings) -> Box<GpuBindGroup>;
    #[native(encoder_create)]
    fn encoder(this: &GpuDevice) -> Box<GpuEncoder>;
    #[native(queue_work_done)]
    #[idl("GPUQueue.onSubmittedWorkDone")]
    fn queueWorkDone(this: &GpuDevice, queue: &GpuQueue) -> Future<()>;
    #[native(texture_create)]
    fn texture(this: &GpuDevice, descriptor: &GpuTextureDescriptor) -> Box<GpuTexture>;
    #[native(pipeline_begin)]
    fn pipeline(this: &GpuDevice) -> Box<GpuPipelineBuilder>;
    #[native(sampler_create)]
    fn sampler(this: &GpuDevice, descriptor: &GpuSamplerDescriptor) -> Box<GpuSampler>;
    #[native(bind_group_layout_create)]
    fn createBindGroupLayout(this: &GpuDevice, descriptor: &GpuBindGroupLayoutDescriptor) -> Box<GpuBindGroupLayout>;
    #[native(pipeline_layout_create)]
    fn createPipelineLayout(this: &GpuDevice, descriptor: &GpuPipelineLayoutDescriptor) -> Box<GpuPipelineLayout>;
    #[native(bind_group_create_with)]
    fn createBindGroup(this: &GpuDevice, descriptor: &GpuBindGroupDescriptor) -> Box<GpuBindGroup>;
    #[native(compute_pipeline_create_with)]
    fn createComputePipeline(this: &GpuDevice, descriptor: &GpuComputePipelineDescriptor) -> Box<GpuPipeline>;
    #[native(compute_pipeline_create_async)]
    fn createComputePipelineAsync(this: &GpuDevice, descriptor: &GpuComputePipelineDescriptor) -> Future<GpuPipeline>;
    #[native(render_pipeline_create_with)]
    fn createRenderPipeline(this: &GpuDevice, descriptor: &GpuRenderPipelineDescriptor) -> Box<GpuPipeline>;
    #[native(render_pipeline_create_async)]
    fn createRenderPipelineAsync(this: &GpuDevice, descriptor: &GpuRenderPipelineDescriptor) -> Future<GpuPipeline>;
    #[native(query_set_create)]
    fn createQuerySet(this: &GpuDevice, descriptor: &GpuQuerySetDescriptor) -> Box<GpuQuerySet>;
    #[native(bundle_encoder_create)]
    fn createRenderBundleEncoder(this: &GpuDevice, descriptor: &GpuRenderBundleEncoderDescriptor) -> Box<GpuRenderBundleEncoder>;
    #[native(device_native_feature)]
    fn supportsNative(this: &GpuDevice, feature: Enum<NativeFeature>) -> bool;
    #[native(device_native_limit)]
    fn nativeLimit(this: &GpuDevice, which: Enum<NativeLimit>) -> i64;
    // Error scopes are per device and per thread, as wgpu keeps them: pop
    // from the task that pushed. The future resolves null when nothing
    // matching the filter went wrong.
    #[native(error_scope_push)]
    fn pushErrorScope(this: &GpuDevice, filter: Enum<ErrorFilter>);
    #[native(error_scope_pop)]
    fn popErrorScope(this: &GpuDevice) -> Future<GpuError>;
    #[native(device_lost)]
    fn lost(this: &GpuDevice) -> Future<GpuDeviceLostInfo>;
    #[native(mesh_pipeline_create)]
    fn createMeshPipeline(this: &GpuDevice, descriptor: &GpuMeshPipelineDescriptor) -> Box<GpuPipeline>;
    #[native(mesh_pipeline_create_async)]
    fn createMeshPipelineAsync(this: &GpuDevice, descriptor: &GpuMeshPipelineDescriptor) -> Future<GpuPipeline>;
    #[native(external_texture_create)]
    fn createExternalTexture(this: &GpuDevice, descriptor: &GpuExternalTextureDescriptor) -> Box<GpuExternalTexture>;
    #[native(blas_create)]
    fn createBlas(this: &GpuDevice, descriptor: &GpuBlasDescriptor) -> Box<GpuBlas>;
    #[native(tlas_create)]
    fn createTlas(this: &GpuDevice, descriptor: &GpuTlasDescriptor) -> Box<GpuTlas>;
    #[native(surface_configure_with)]
    fn configureSurfaceWith(this: &GpuDevice, surface: &GpuSurface, configuration: &GpuSurfaceConfiguration);
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
    #[native(queue_write_texture_with)]
    fn writeTextureWith(this: &GpuQueue, destination: &GpuTexelCopyTextureInfo, data: Buffer, layout: &GpuTexelCopyBufferLayout, size: &GpuExtent3D);
    // Nanoseconds per timestamp query tick.
    #[native(queue_timestamp_period)]
    fn timestampPeriod(this: &GpuQueue) -> f64;
    // A compacted copy of a BLAS whose prepareCompaction has resolved.
    #[native(queue_compact_blas)]
    fn compactBlas(this: &GpuQueue, blas: &GpuBlas) -> Box<GpuBlas>;
}

#[idl("GPUBuffer")]
trait GpuBuffer {
    #[native(is_valid)]
    fn valid(this: &GpuBuffer) -> bool;
    #[native(buffer_copy_out)]
    fn copyOut(this: &GpuBuffer, offset: i64, out: Buffer, len: i32) -> bool;
    // Into a range mapped for writing, or mapped at creation.
    #[native(buffer_copy_in)]
    fn copyIn(this: &GpuBuffer, offset: i64, data: Buffer, len: i32) -> bool;
    #[native(buffer_size)]
    fn size(this: &GpuBuffer) -> i64;
    #[native(buffer_usage)]
    fn usage(this: &GpuBuffer) -> i32;
    #[native(buffer_unmap)]
    fn unmap(this: &GpuBuffer);
    #[native(buffer_destroy)]
    fn destroy(this: &GpuBuffer);
}

#[idl("GPUShaderModule")]
trait GpuShader {
    #[native(is_valid)]
    fn valid(this: &GpuShader) -> bool;
    #[native(shader_destroy)]
    fn destroy(this: &GpuShader);
    #[native(shader_messages)]
    fn messages(this: &GpuShader) -> Text;
    #[native(shader_compilation_info)]
    fn getCompilationInfo(this: &GpuShader) -> Future<GpuCompilationInfo>;
}

trait GpuPipeline {
    #[native(is_valid)]
    fn valid(this: &GpuPipeline) -> bool;
    #[native(pipeline_release)]
    fn destroy(this: &GpuPipeline);
    #[native(pipeline_bind_group_layout)]
    fn getBindGroupLayout(this: &GpuPipeline, index: i32) -> Box<GpuBindGroupLayout>;
}

#[idl("GPUBindGroupLayout")]
trait GpuBindGroupLayout {
    #[native(is_valid)]
    fn valid(this: &GpuBindGroupLayout) -> bool;
    #[native(bind_group_layout_destroy)]
    fn destroy(this: &GpuBindGroupLayout);
}

#[idl("GPUPipelineLayout")]
trait GpuPipelineLayout {
    #[native(is_valid)]
    fn valid(this: &GpuPipelineLayout) -> bool;
    #[native(pipeline_layout_destroy)]
    fn destroy(this: &GpuPipelineLayout);
}

#[idl("GPUBindGroup")]
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
    // Dynamic offsets as WebGPU's Uint32Array form: `count` offsets from
    // element `start` of `offsets`, one per dynamic binding in binding order.
    #[native(render_set_bind_group_offsets)]
    fn renderSetBindGroupOffsets(this: &GpuEncoder, group: i32, bindgroup: &GpuBindGroup, offsets: Buffer, start: i64, count: i32);
    // A compute pass open on the encoder until computeEnd, as a render pass
    // is until renderEnd; compute() is the one-dispatch shorthand.
    #[native(compute_begin)]
    fn computeBegin(this: &GpuEncoder);
    #[native(compute_set_pipeline)]
    fn computeSetPipeline(this: &GpuEncoder, pipeline: &GpuPipeline);
    #[native(compute_set_bind_group)]
    fn computeSetBindGroup(this: &GpuEncoder, group: i32, bindgroup: &GpuBindGroup);
    #[native(compute_set_bind_group_offsets)]
    fn computeSetBindGroupOffsets(this: &GpuEncoder, group: i32, bindgroup: &GpuBindGroup, offsets: Buffer, start: i64, count: i32);
    #[native(compute_dispatch)]
    fn computeDispatch(this: &GpuEncoder, x: i32, y: i32, z: i32);
    #[native(compute_dispatch_indirect)]
    fn computeDispatchIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64);
    #[native(compute_end)]
    fn computeEnd(this: &GpuEncoder);
    // Passes from their full descriptors.
    #[native(render_pass_begin_with)]
    fn beginRenderPass(this: &GpuEncoder, descriptor: &GpuRenderPassDescriptor);
    #[native(compute_pass_begin_with)]
    fn beginComputePass(this: &GpuEncoder, descriptor: &GpuComputePassDescriptor);
    // Draws and buffers with their full ranges. A negative size is the rest
    // of the buffer.
    #[native(render_draw_range)]
    fn renderDrawRange(this: &GpuEncoder, vertex_count: i32, instance_count: i32, first_vertex: i32, first_instance: i32);
    #[native(render_draw_indexed_range)]
    fn renderDrawIndexedRange(this: &GpuEncoder, index_count: i32, instance_count: i32, first_index: i32, base_vertex: i32, first_instance: i32);
    #[native(render_set_vertex_buffer_range)]
    fn renderSetVertexBufferRange(this: &GpuEncoder, slot: i32, buffer: &GpuBuffer, offset: i64, size: i64);
    #[native(render_set_index_buffer_range)]
    fn renderSetIndexBufferRange(this: &GpuEncoder, buffer: &GpuBuffer, format: Enum<IndexFormat>, offset: i64, size: i64);
    // wgpu's own, with MULTI_DRAW_INDIRECT_COUNT for the counted forms.
    #[native(render_multi_draw_indirect)]
    fn renderMultiDrawIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, count: i32);
    #[native(render_multi_draw_indexed_indirect)]
    fn renderMultiDrawIndexedIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, count: i32);
    #[native(render_multi_draw_indirect_count)]
    fn renderMultiDrawIndirectCount(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, count_buffer: &GpuBuffer, count_offset: i64, max_count: i32);
    #[native(render_multi_draw_indexed_indirect_count)]
    fn renderMultiDrawIndexedIndirectCount(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, count_buffer: &GpuBuffer, count_offset: i64, max_count: i32);
    // Immediate data (WebGPU's setImmediates), `size` bytes from `start` of
    // a shared buffer, at byte `offset` of the pipeline's immediates.
    #[native(render_set_immediates)]
    fn renderSetImmediates(this: &GpuEncoder, offset: i32, data: Buffer, start: i64, size: i32);
    #[native(compute_set_immediates)]
    fn computeSetImmediates(this: &GpuEncoder, offset: i32, data: Buffer, start: i64, size: i32);
    #[native(render_begin_occlusion_query)]
    fn renderBeginOcclusionQuery(this: &GpuEncoder, index: i32);
    #[native(render_end_occlusion_query)]
    fn renderEndOcclusionQuery(this: &GpuEncoder);
    #[native(render_begin_pipeline_statistics)]
    fn renderBeginPipelineStatisticsQuery(this: &GpuEncoder, query_set: &GpuQuerySet, index: i32);
    #[native(render_end_pipeline_statistics)]
    fn renderEndPipelineStatisticsQuery(this: &GpuEncoder);
    #[native(compute_begin_pipeline_statistics)]
    fn computeBeginPipelineStatisticsQuery(this: &GpuEncoder, query_set: &GpuQuerySet, index: i32);
    #[native(compute_end_pipeline_statistics)]
    fn computeEndPipelineStatisticsQuery(this: &GpuEncoder);
    // wgpu's own: TIMESTAMP_QUERY_INSIDE_ENCODERS and _INSIDE_PASSES.
    #[native(encoder_write_timestamp)]
    fn writeTimestamp(this: &GpuEncoder, query_set: &GpuQuerySet, index: i32);
    #[native(render_write_timestamp)]
    fn renderWriteTimestamp(this: &GpuEncoder, query_set: &GpuQuerySet, index: i32);
    #[native(compute_write_timestamp)]
    fn computeWriteTimestamp(this: &GpuEncoder, query_set: &GpuQuerySet, index: i32);
    #[native(encoder_resolve_query_set)]
    fn resolveQuerySet(this: &GpuEncoder, query_set: &GpuQuerySet, first: i32, count: i32, destination: &GpuBuffer, offset: i64);
    #[native(render_execute_bundle)]
    fn renderExecuteBundle(this: &GpuEncoder, bundle: &GpuRenderBundle);
    #[native(render_draw_mesh_tasks)]
    fn renderDrawMeshTasks(this: &GpuEncoder, x: i32, y: i32, z: i32);
    #[native(render_draw_mesh_tasks_indirect)]
    fn renderDrawMeshTasksIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64);
    #[native(render_multi_draw_mesh_tasks_indirect)]
    fn renderMultiDrawMeshTasksIndirect(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, count: i32);
    #[native(render_multi_draw_mesh_tasks_indirect_count)]
    fn renderMultiDrawMeshTasksIndirectCount(this: &GpuEncoder, buffer: &GpuBuffer, offset: i64, count_buffer: &GpuBuffer, count_offset: i64, max_count: i32);
    // Outside any pass. A TLAS's instances are the ones set when this runs.
    #[native(encoder_build_acceleration_structures)]
    fn buildAccelerationStructures(this: &GpuEncoder, build: &GpuAccelerationStructureBuild);
    #[native(encoder_copy_buffer_to_texture_with)]
    fn copyBufferToTextureWith(this: &GpuEncoder, source: &GpuTexelCopyBufferInfo, destination: &GpuTexelCopyTextureInfo, size: &GpuExtent3D);
    #[native(encoder_copy_texture_to_buffer_with)]
    fn copyTextureToBufferWith(this: &GpuEncoder, source: &GpuTexelCopyTextureInfo, destination: &GpuTexelCopyBufferInfo, size: &GpuExtent3D);
    #[native(encoder_copy_texture_to_texture_with)]
    fn copyTextureToTextureWith(this: &GpuEncoder, source: &GpuTexelCopyTextureInfo, destination: &GpuTexelCopyTextureInfo, size: &GpuExtent3D);
    // wgpu's own, with CLEAR_TEXTURE: every mip level and layer to zero.
    #[native(encoder_clear_texture)]
    fn clearTexture(this: &GpuEncoder, texture: &GpuTexture);
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

#[idl("GPUTexture")]
trait GpuTexture {
    #[native(is_valid)]
    fn valid(this: &GpuTexture) -> bool;
    #[native(texture_view)]
    fn createView(this: &GpuTexture, descriptor: &GpuTextureViewDescriptor) -> Box<GpuTextureView>;
    #[native(texture_destroy)]
    fn destroy(this: &GpuTexture);
    #[native(texture_width)]
    fn width(this: &GpuTexture) -> i32;
    #[native(texture_height)]
    fn height(this: &GpuTexture) -> i32;
    #[native(texture_depth_or_array_layers)]
    fn depthOrArrayLayers(this: &GpuTexture) -> i32;
    #[native(texture_mip_level_count)]
    fn mipLevelCount(this: &GpuTexture) -> i32;
    #[native(texture_sample_count)]
    fn sampleCount(this: &GpuTexture) -> i32;
    #[native(texture_get_dimension)]
    fn dimension(this: &GpuTexture) -> Enum<TextureDimension>;
    #[native(texture_get_format)]
    fn format(this: &GpuTexture) -> Enum<TextureFormat>;
    #[native(texture_usage)]
    fn usage(this: &GpuTexture) -> i32;
}

#[idl("GPUTextureView")]
trait GpuTextureView {
    #[native(is_valid)]
    fn valid(this: &GpuTextureView) -> bool;
    #[native(view_destroy)]
    fn destroy(this: &GpuTextureView);
}

#[idl("GPUSampler")]
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
    #[native(pipeline_layout)]
    fn layout(this: &GpuPipelineBuilder, layout: &GpuPipelineLayout);
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
    #[native(surface_capabilities)]
    fn capabilities(this: &GpuSurface, adapter: &GpuAdapter) -> Box<GpuSurfaceCapabilities>;
}

#[idl("GPUQuerySet")]
trait GpuQuerySet {
    #[native(is_valid)]
    fn valid(this: &GpuQuerySet) -> bool;
    #[native(query_set_destroy)]
    fn destroy(this: &GpuQuerySet);
    #[native(query_set_count)]
    fn count(this: &GpuQuerySet) -> i32;
    #[native(query_set_type)]
    fn queryType(this: &GpuQuerySet) -> Enum<QueryType>;
}

// Recorded commands replayed into wgpu's bundle encoder by finish(), which
// consumes the encoder.
#[idl("GPURenderBundleEncoder")]
trait GpuRenderBundleEncoder {
    #[native(is_valid)]
    fn valid(this: &GpuRenderBundleEncoder) -> bool;
    #[native(bundle_encoder_destroy)]
    fn destroy(this: &GpuRenderBundleEncoder);
    #[native(bundle_set_pipeline)]
    fn setPipeline(this: &GpuRenderBundleEncoder, pipeline: &GpuPipeline);
    #[native(bundle_set_bind_group)]
    fn setBindGroup(this: &GpuRenderBundleEncoder, group: i32, bindgroup: &GpuBindGroup);
    #[native(bundle_set_bind_group_offsets)]
    fn setBindGroupOffsets(this: &GpuRenderBundleEncoder, group: i32, bindgroup: &GpuBindGroup, offsets: Buffer, start: i64, count: i32);
    #[native(bundle_set_vertex_buffer)]
    fn setVertexBuffer(this: &GpuRenderBundleEncoder, slot: i32, buffer: &GpuBuffer, offset: i64, size: i64);
    #[native(bundle_set_index_buffer)]
    fn setIndexBuffer(this: &GpuRenderBundleEncoder, buffer: &GpuBuffer, format: Enum<IndexFormat>, offset: i64, size: i64);
    #[native(bundle_draw)]
    fn draw(this: &GpuRenderBundleEncoder, vertex_count: i32, instance_count: i32, first_vertex: i32, first_instance: i32);
    #[native(bundle_draw_indexed)]
    fn drawIndexed(this: &GpuRenderBundleEncoder, index_count: i32, instance_count: i32, first_index: i32, base_vertex: i32, first_instance: i32);
    #[native(bundle_draw_indirect)]
    fn drawIndirect(this: &GpuRenderBundleEncoder, buffer: &GpuBuffer, offset: i64);
    #[native(bundle_draw_indexed_indirect)]
    fn drawIndexedIndirect(this: &GpuRenderBundleEncoder, buffer: &GpuBuffer, offset: i64);
    #[native(bundle_finish)]
    fn finish(this: &GpuRenderBundleEncoder) -> Box<GpuRenderBundle>;
}

#[idl("GPURenderBundle")]
trait GpuRenderBundle {
    #[native(is_valid)]
    fn valid(this: &GpuRenderBundle) -> bool;
    #[native(bundle_destroy)]
    fn destroy(this: &GpuRenderBundle);
}

// What popErrorScope reports.
trait GpuError {
    #[native(is_valid)]
    fn valid(this: &GpuError) -> bool;
    #[native(error_destroy)]
    fn destroy(this: &GpuError);
    #[native(error_filter)]
    fn filter(this: &GpuError) -> Enum<ErrorFilter>;
    #[native(error_message)]
    fn message(this: &GpuError) -> Text;
}

#[idl("GPUDeviceLostInfo")]
trait GpuDeviceLostInfo {
    #[native(is_valid)]
    fn valid(this: &GpuDeviceLostInfo) -> bool;
    #[native(lost_destroy)]
    fn destroy(this: &GpuDeviceLostInfo);
    #[native(lost_reason)]
    fn reason(this: &GpuDeviceLostInfo) -> Enum<DeviceLostReason>;
    #[native(lost_message)]
    fn message(this: &GpuDeviceLostInfo) -> Text;
}

// The compiler's messages about a shader, each with its source location.
// Line and column count from 1; offset and length are in UTF-16 units, as
// WebGPU reports them.
#[idl("GPUCompilationInfo")]
trait GpuCompilationInfo {
    #[native(is_valid)]
    fn valid(this: &GpuCompilationInfo) -> bool;
    #[native(compilation_destroy)]
    fn destroy(this: &GpuCompilationInfo);
    #[native(compilation_count)]
    fn messageCount(this: &GpuCompilationInfo) -> i32;
    #[native(compilation_message)]
    fn message(this: &GpuCompilationInfo, index: i32) -> Text;
    #[native(compilation_type)]
    fn messageType(this: &GpuCompilationInfo, index: i32) -> Enum<CompilationMessageType>;
    #[native(compilation_line)]
    fn lineNum(this: &GpuCompilationInfo, index: i32) -> i64;
    #[native(compilation_column)]
    fn linePos(this: &GpuCompilationInfo, index: i32) -> i64;
    #[native(compilation_offset)]
    fn offset(this: &GpuCompilationInfo, index: i32) -> i64;
    #[native(compilation_length)]
    fn length(this: &GpuCompilationInfo, index: i32) -> i64;
}

// What a surface supports on an adapter.
trait GpuSurfaceCapabilities {
    #[native(is_valid)]
    fn valid(this: &GpuSurfaceCapabilities) -> bool;
    #[native(capabilities_destroy)]
    fn destroy(this: &GpuSurfaceCapabilities);
    #[native(capabilities_format_count)]
    fn formatCount(this: &GpuSurfaceCapabilities) -> i32;
    #[native(capabilities_format)]
    fn format(this: &GpuSurfaceCapabilities, index: i32) -> Enum<TextureFormat>;
    #[native(capabilities_present_mode_count)]
    fn presentModeCount(this: &GpuSurfaceCapabilities) -> i32;
    #[native(capabilities_present_mode)]
    fn presentMode(this: &GpuSurfaceCapabilities, index: i32) -> Enum<PresentMode>;
    #[native(capabilities_alpha_mode_count)]
    fn alphaModeCount(this: &GpuSurfaceCapabilities) -> i32;
    #[native(capabilities_alpha_mode)]
    fn alphaMode(this: &GpuSurfaceCapabilities, index: i32) -> Enum<AlphaMode>;
    #[native(capabilities_usages)]
    fn usages(this: &GpuSurfaceCapabilities) -> i32;
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

#[idl("GPUExternalTexture")]
trait GpuExternalTexture {
    #[native(is_valid)]
    fn valid(this: &GpuExternalTexture) -> bool;
    #[native(external_texture_destroy)]
    fn destroy(this: &GpuExternalTexture);
}

trait GpuBlas {
    #[native(is_valid)]
    fn valid(this: &GpuBlas) -> bool;
    #[native(blas_destroy)]
    fn destroy(this: &GpuBlas);
    // Resolves once the BLAS's builds finish and it can be compacted.
    #[native(blas_prepare_compaction)]
    fn prepareCompaction(this: &GpuBlas) -> Future<()>;
    #[native(blas_ready_for_compaction)]
    fn readyForCompaction(this: &GpuBlas) -> bool;
}

trait GpuTlas {
    #[native(is_valid)]
    fn valid(this: &GpuTlas) -> bool;
    #[native(tlas_destroy)]
    fn destroy(this: &GpuTlas);
    #[native(tlas_max_instances)]
    fn maxInstances(this: &GpuTlas) -> i32;
    #[native(tlas_set_instance)]
    fn setInstance(this: &GpuTlas, index: i32, instance: &GpuTlasInstance);
    #[native(tlas_clear_instance)]
    fn clearInstance(this: &GpuTlas, index: i32);
}
