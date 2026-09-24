//! Native GPU operations, adapted from hlwgpu (see LICENSE.hlwgpu).
//!
//! Uses Caribou Text and shared Buffer carriers, with typed generated objects.
//! Nothing here is generated; `bindings` is the part that is.

// gpu.api.rs names these operations and generates their typed ABI wrappers.
#![allow(clippy::too_many_arguments)]

use std::collections::VecDeque;
use std::future::Future as StdFuture;
use std::sync::{Arc, LazyLock, Mutex};

use crate::handles::{Slab, kind_of};
use crate::types::Kind;
use crate::{
    BindingResource, GpuBindGroupDescriptor, GpuBindGroupLayoutDescriptor, GpuBindGroupLayoutEntry,
    GpuBufferDescriptor, GpuDeviceDescriptor, GpuPipelineLayoutDescriptor, GpuSamplerDescriptor,
    GpuTextureDescriptor, GpuTextureViewDescriptor,
};
use caribou_abi::{Buffer, ErrorKind, Future, Rooted, Text, Value, host};

/// A device and the queue that came back with it.
struct DeviceEntry {
    device: wgpu::Device,
    queue: i32,
    /// What the device has complained about and nobody has collected.
    ///
    /// wgpu's default for an uncaptured error is to panic, which takes the
    /// process with it: a wrong surface format killed a program here with a
    /// message naming neither the cause nor the caller. A queue instead, and
    /// `device_take_error` hands them over.
    errors: Arc<Mutex<VecDeque<String>>>,
    /// Whether the device is lost, and whose futures wait to hear it.
    lost: Arc<Mutex<diagnostics::Lost>>,
}

/// An encoder and whatever pass is open on it.
///
/// A `RenderPass` borrows its encoder, which a handle table cannot express, so
/// `forget_lifetime` erases it and the two are kept together instead. The
/// encoder is `Option` because `finish()` consumes it.
#[derive(Default)]
struct EncoderEntry {
    encoder: Option<wgpu::CommandEncoder>,
    pass: Option<wgpu::RenderPass<'static>>,
    /// The open compute pass, the same way. At most one of the two is open.
    compute: Option<wgpu::ComputePass<'static>>,
    /// What the next pass will attach, in the order it was described. Held as
    /// handles rather than views because the views have to outlive the
    /// descriptor, and that is easier to arrange when the pass opens.
    colour: Vec<(i32, wgpu::Color)>,
    depth: Option<(i32, f64, i32)>,
}

impl Drop for EncoderEntry {
    fn drop(&mut self) {
        // A forgotten pass lifetime still requires ending the pass before
        // releasing its encoder.
        self.pass.take();
        self.compute.take();
        self.encoder.take();
    }
}
type Encoder = Mutex<EncoderEntry>;

/// Native locks protect GPU objects and callbacks from backend threads.
macro_rules! slab {
    ($name:ident, $ty:ty, $kind:expr) => {
        static $name: LazyLock<Mutex<Slab<$ty>>> = LazyLock::new(|| Mutex::new(Slab::new($kind)));
    };
}

slab!(INSTANCES, wgpu::Instance, Kind::Instance);
slab!(ADAPTERS, wgpu::Adapter, Kind::Adapter);
slab!(DEVICES, DeviceEntry, Kind::Device);
slab!(QUEUES, wgpu::Queue, Kind::Queue);
slab!(BUFFERS, wgpu::Buffer, Kind::Buffer);
slab!(SHADERS, wgpu::ShaderModule, Kind::Shader);
slab!(PIPELINES, wgpu::ComputePipeline, Kind::Pipeline);
slab!(RENDER_PIPELINES, wgpu::RenderPipeline, Kind::Renderpipeline);
slab!(TEXTURES, wgpu::Texture, Kind::Texture);
slab!(VIEWS, wgpu::TextureView, Kind::View);
slab!(SAMPLERS, wgpu::Sampler, Kind::Sampler);
slab!(BINDGROUPS, wgpu::BindGroup, Kind::Bindgroup);
slab!(ENCODERS, Encoder, Kind::Encoder);

/// Looks a handle up and lets go of the slab before the object is used, so no
/// two of these locks are ever held at once.
macro_rules! find {
    ($slab:ident, $handle:expr) => {
        match $slab.lock().unwrap().get($handle) {
            Some(found) => found,
            None => return Default::default(),
        }
    };
    ($slab:ident, $handle:expr, $miss:expr) => {
        match $slab.lock().unwrap().get($handle) {
            Some(found) => found,
            None => return $miss,
        }
    };
}

// Text uses Caribou's UTF-8 view. It is never retained in a native callback.
fn text_out(text: &str) -> Text {
    Text::new(text)
}

/// Validate before borrowing shared storage. No byte copy or HL allocation.
fn bytes(data: &Buffer, len: i32) -> Option<&[u8]> {
    let Ok(len) = usize::try_from(len) else {
        host::raise(ErrorKind::Type, "negative byte length");
        return None;
    };
    if len > data.len() {
        host::raise(ErrorKind::Type, "byte length exceeds shared buffer");
        return None;
    }
    Some(unsafe { &data.as_slice()[..len] })
}

/// What went wrong, in full: wgpu's Display for an error names only its
/// kind; the description carries the cause.
fn error_text(error: &wgpu::Error) -> String {
    match error {
        wgpu::Error::Validation { description, .. } | wgpu::Error::Internal { description, .. } => {
            description.clone()
        }
        wgpu::Error::OutOfMemory { source } => format!("Out of memory: {source}"),
    }
}

fn rejected_future<T>(message: &str) -> Future<T> {
    let future = Future::new();
    future.reject(Text::new(message).value());
    future
}

#[cfg(not(all(target_arch = "wasm32", not(target_os = "emscripten"))))]
fn spawn_gpu(work: impl StdFuture<Output = ()> + Send + 'static) {
    std::thread::spawn(move || pollster::block_on(work));
}

#[cfg(all(target_arch = "wasm32", not(target_os = "emscripten")))]
fn spawn_gpu(work: impl StdFuture<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(work);
}

/// Native callbacks only run when wgpu is polled. Browser WebGPU is driven by
/// its event loop; native backends get a short-lived waiter so awaiting a
/// Caribou future never requires a language-side busy loop.
fn drive_device(device: wgpu::Device) {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(move || {
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    });
    #[cfg(target_arch = "wasm32")]
    let _ = device;
}

// -- instance ---------------------------------------------------------------

pub unsafe fn instance_create() -> i32 {
    // `with_env` honours WGPU_BACKEND and the rest, so a program built with
    // more than one backend can be told which to use without an API for it.
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle().with_env());
    INSTANCES.lock().unwrap().put(instance)
}

pub unsafe fn instance_create_with(descriptor: &crate::GpuInstanceDescriptor) -> i32 {
    let mut chosen = wgpu::InstanceDescriptor::new_without_display_handle();
    if let Some(backends) = descriptor.backends {
        chosen.backends = wgpu::Backends::from_bits_truncate(backends as u32);
    }
    if let Some(flags) = descriptor.flags {
        chosen.flags = wgpu::InstanceFlags::from_bits_truncate(flags as u32);
    }
    INSTANCES.lock().unwrap().put(wgpu::Instance::new(chosen))
}

pub unsafe fn instance_destroy(inst: i32) {
    INSTANCES.lock().unwrap().remove(inst);
}

// -- adapter ----------------------------------------------------------------

fn power_preference(power: i32) -> wgpu::PowerPreference {
    match power {
        1 => wgpu::PowerPreference::HighPerformance,
        0 => wgpu::PowerPreference::LowPower,
        _ => wgpu::PowerPreference::None,
    }
}

/// Resolves with the adapter a started request finds.
fn settle_adapter(
    request: impl StdFuture<Output = Result<wgpu::Adapter, wgpu::RequestAdapterError>>
    + wgpu::WasmNotSend
    + 'static,
) -> Future<crate::GpuAdapter> {
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        match request.await {
            Ok(adapter) => {
                let handle = ADAPTERS.lock().unwrap().put(adapter);
                if handle == 0 {
                    completion
                        .get()
                        .reject(Text::new("adapter resource table is full").value());
                    return;
                }
                if !completion
                    .get()
                    .resolve_boxed(Box::new(crate::GpuAdapter { handle }))
                {
                    ADAPTERS.lock().unwrap().remove(handle);
                }
            }
            Err(error) => {
                completion
                    .get()
                    .reject(Text::new(&error.to_string()).value());
            }
        }
    });
    future
}

pub unsafe fn adapter_request(inst: i32, power: i32) -> Future<crate::GpuAdapter> {
    let Some(instance) = INSTANCES.lock().unwrap().get(inst) else {
        return rejected_future("instance was destroyed");
    };
    settle_adapter(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: power_preference(power),
        ..Default::default()
    }))
}

/// The request starts while the surface is held; wgpu's request future
/// borrows neither the instance nor the options.
pub unsafe fn adapter_request_with(
    inst: i32,
    options: &crate::GpuRequestAdapterOptions,
) -> Future<crate::GpuAdapter> {
    let Some(instance) = INSTANCES.lock().unwrap().get(inst) else {
        return rejected_future("instance was destroyed");
    };
    let surface = match options.compatibleSurface {
        None => None,
        Some(handle) => match SURFACES.lock().unwrap().get(handle) {
            Some(surface) => Some(surface),
            None => return rejected_future("the compatible surface was destroyed"),
        },
    };
    let held = surface.as_ref().map(|surface| surface.lock().unwrap());
    let request = instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: power_preference(options.powerPreference.unwrap_or(-1)),
        force_fallback_adapter: options.forceFallbackAdapter.unwrap_or(false),
        compatible_surface: held.as_ref().map(|entry| &entry.surface),
        ..Default::default()
    });
    drop(held);
    settle_adapter(request)
}

pub unsafe fn adapter_name(adapter: i32) -> Text {
    let adapter = find!(ADAPTERS, adapter, Text::NULL);
    text_out(&adapter.get_info().name)
}

pub unsafe fn adapter_backend(adapter: i32) -> i32 {
    let adapter = find!(ADAPTERS, adapter, 0);
    match adapter.get_info().backend {
        wgpu::Backend::Vulkan => 1,
        wgpu::Backend::Metal => 2,
        wgpu::Backend::Dx12 => 3,
        wgpu::Backend::Gl => 4,
        wgpu::Backend::BrowserWebGpu => 5,
        _ => 0,
    }
}

/// WebGPU feature ordinal to wgpu's capability bit.
fn feature(which: i32) -> Option<wgpu::Features> {
    Some(match which {
        // wgpu 30 predates the explicit core-features-and-limits feature.
        0 => return None,
        1 => wgpu::Features::DEPTH_CLIP_CONTROL,
        2 => wgpu::Features::DEPTH32FLOAT_STENCIL8,
        3 => wgpu::Features::TEXTURE_COMPRESSION_BC,
        4 => wgpu::Features::TEXTURE_COMPRESSION_BC_SLICED_3D,
        5 => wgpu::Features::TEXTURE_COMPRESSION_ETC2,
        6 => wgpu::Features::TEXTURE_COMPRESSION_ASTC,
        7 => wgpu::Features::TEXTURE_COMPRESSION_ASTC_SLICED_3D,
        8 => wgpu::Features::TIMESTAMP_QUERY,
        9 => wgpu::Features::INDIRECT_FIRST_INSTANCE,
        10 => wgpu::Features::SHADER_F16,
        11 => wgpu::Features::RG11B10UFLOAT_RENDERABLE,
        12 => wgpu::Features::BGRA8UNORM_STORAGE,
        13 => wgpu::Features::FLOAT32_FILTERABLE,
        14 => wgpu::Features::FLOAT32_BLENDABLE,
        15 => wgpu::Features::CLIP_DISTANCES,
        16 => wgpu::Features::DUAL_SOURCE_BLENDING,
        17 => wgpu::Features::SUBGROUP,
        20 => wgpu::Features::PRIMITIVE_INDEX,
        // The remaining WebGPU draft features have no wgpu 30 equivalent.
        18 | 19 | 21 | 22 | 23 => return None,
        _ => return None,
    })
}

fn supports(features: wgpu::Features, which: i32) -> bool {
    match feature(which) {
        Some(required) => required.is_empty() || features.contains(required),
        None => false,
    }
}

pub unsafe fn adapter_feature(adapter: i32, which: i32) -> bool {
    let adapter = find!(ADAPTERS, adapter, false);
    supports(adapter.features(), which)
}

pub unsafe fn device_feature(device: i32, which: i32) -> bool {
    let device = find!(DEVICES, device, false);
    supports(device.device.features(), which)
}

fn limit_value(limits: &wgpu::Limits, which: i32) -> Option<i64> {
    let value = match which {
        0 => limits.max_texture_dimension_1d as u64,
        1 => limits.max_texture_dimension_2d as u64,
        2 => limits.max_texture_dimension_3d as u64,
        3 => limits.max_texture_array_layers as u64,
        4 => limits.max_bind_groups as u64,
        5 => limits.max_bind_groups_plus_vertex_buffers as u64,
        6 => limits.max_immediate_size as u64,
        7 => limits.max_bindings_per_bind_group as u64,
        8 => limits.max_dynamic_uniform_buffers_per_pipeline_layout as u64,
        9 => limits.max_dynamic_storage_buffers_per_pipeline_layout as u64,
        10 => limits.max_sampled_textures_per_shader_stage as u64,
        11 => limits.max_samplers_per_shader_stage as u64,
        12 => limits.max_storage_buffers_per_shader_stage as u64,
        13 | 14 => return None,
        15 => limits.max_storage_textures_per_shader_stage as u64,
        16 | 17 => return None,
        18 => limits.max_uniform_buffers_per_shader_stage as u64,
        19 => limits.max_uniform_buffer_binding_size,
        20 => limits.max_storage_buffer_binding_size,
        21 => limits.min_uniform_buffer_offset_alignment as u64,
        22 => limits.min_storage_buffer_offset_alignment as u64,
        23 => limits.max_vertex_buffers as u64,
        24 => limits.max_buffer_size,
        25 => limits.max_vertex_attributes as u64,
        26 => limits.max_vertex_buffer_array_stride as u64,
        27 => limits.max_inter_stage_shader_variables as u64,
        28 => limits.max_color_attachments as u64,
        29 => limits.max_color_attachment_bytes_per_sample as u64,
        30 => limits.max_compute_workgroup_storage_size as u64,
        31 => limits.max_compute_invocations_per_workgroup as u64,
        32 => limits.max_compute_workgroup_size_x as u64,
        33 => limits.max_compute_workgroup_size_y as u64,
        34 => limits.max_compute_workgroup_size_z as u64,
        35 => limits.max_compute_workgroups_per_dimension as u64,
        _ => return None,
    };
    Some(value.min(i64::MAX as u64) as i64)
}

pub unsafe fn adapter_limit(adapter: i32, which: i32) -> i64 {
    let adapter = find!(ADAPTERS, adapter, 0);
    limit_value(&adapter.limits(), which).unwrap_or(-1)
}

pub unsafe fn device_limit(device: i32, which: i32) -> i64 {
    let device = find!(DEVICES, device, 0);
    limit_value(&device.device.limits(), which).unwrap_or(-1)
}

pub unsafe fn adapter_destroy(adapter: i32) {
    ADAPTERS.lock().unwrap().remove(adapter);
}

// -- device -----------------------------------------------------------------

fn requested_features(requested: &[i32]) -> Result<wgpu::Features, String> {
    let mut enabled = wgpu::Features::empty();
    for &which in requested {
        let Some(value) = feature(which) else {
            return Err(format!(
                "requested WebGPU feature {which} is unavailable in wgpu 30"
            ));
        };
        enabled |= value;
    }
    Ok(enabled)
}

fn requested_limits(requested: &[(i32, i64)]) -> Result<wgpu::Limits, String> {
    fn u32_value(value: i64) -> Result<u32, String> {
        u32::try_from(value).map_err(|_| format!("GPU limit value {value} is outside u32"))
    }
    fn u64_value(value: i64) -> Result<u64, String> {
        u64::try_from(value).map_err(|_| format!("GPU limit value {value} is negative"))
    }

    let defaults = wgpu::Limits::default();
    let mut limits = defaults.clone();
    for &(which, value) in requested {
        match which {
            0 => limits.max_texture_dimension_1d = u32_value(value)?,
            1 => limits.max_texture_dimension_2d = u32_value(value)?,
            2 => limits.max_texture_dimension_3d = u32_value(value)?,
            3 => limits.max_texture_array_layers = u32_value(value)?,
            4 => limits.max_bind_groups = u32_value(value)?,
            5 => limits.max_bind_groups_plus_vertex_buffers = u32_value(value)?,
            6 => limits.max_immediate_size = u32_value(value)?,
            7 => limits.max_bindings_per_bind_group = u32_value(value)?,
            8 => limits.max_dynamic_uniform_buffers_per_pipeline_layout = u32_value(value)?,
            9 => limits.max_dynamic_storage_buffers_per_pipeline_layout = u32_value(value)?,
            10 => limits.max_sampled_textures_per_shader_stage = u32_value(value)?,
            11 => limits.max_samplers_per_shader_stage = u32_value(value)?,
            12 => limits.max_storage_buffers_per_shader_stage = u32_value(value)?,
            13 | 14 => {
                return Err("per-stage storage-buffer limits are unavailable in wgpu 30".into());
            }
            15 => limits.max_storage_textures_per_shader_stage = u32_value(value)?,
            16 | 17 => {
                return Err("per-stage storage-texture limits are unavailable in wgpu 30".into());
            }
            18 => limits.max_uniform_buffers_per_shader_stage = u32_value(value)?,
            19 => limits.max_uniform_buffer_binding_size = u64_value(value)?,
            20 => limits.max_storage_buffer_binding_size = u64_value(value)?,
            21 => limits.min_uniform_buffer_offset_alignment = u32_value(value)?,
            22 => limits.min_storage_buffer_offset_alignment = u32_value(value)?,
            23 => limits.max_vertex_buffers = u32_value(value)?,
            24 => limits.max_buffer_size = u64_value(value)?,
            25 => limits.max_vertex_attributes = u32_value(value)?,
            26 => limits.max_vertex_buffer_array_stride = u32_value(value)?,
            27 => limits.max_inter_stage_shader_variables = u32_value(value)?,
            28 => limits.max_color_attachments = u32_value(value)?,
            29 => limits.max_color_attachment_bytes_per_sample = u32_value(value)?,
            30 => limits.max_compute_workgroup_storage_size = u32_value(value)?,
            31 => limits.max_compute_invocations_per_workgroup = u32_value(value)?,
            32 => limits.max_compute_workgroup_size_x = u32_value(value)?,
            33 => limits.max_compute_workgroup_size_y = u32_value(value)?,
            34 => limits.max_compute_workgroup_size_z = u32_value(value)?,
            35 => limits.max_compute_workgroups_per_dimension = u32_value(value)?,
            _ => return Err(format!("unknown GPU limit {which}")),
        }
    }
    Ok(limits.or_better_values_from(&defaults))
}

fn device_request_configured(
    adapter: i32,
    features: &[i32],
    limits: &[(i32, i64)],
    native_features: &[i32],
    native_limits: &[(i32, i64)],
    memory_hints: Option<i32>,
) -> Future<crate::GpuDevice> {
    let Some(adapter) = ADAPTERS.lock().unwrap().get(adapter) else {
        return rejected_future("adapter was destroyed");
    };
    let requested_features = match requested_features(features)
        .and_then(|webgpu| Ok(webgpu | native::requested_features(native_features)?))
    {
        Ok(features) => features,
        Err(error) => return rejected_future(&error),
    };
    let requested_limits = match requested_limits(limits)
        .and_then(|limits| native::requested_limits(limits, native_limits))
    {
        Ok(limits) => limits,
        Err(error) => return rejected_future(&error),
    };
    // Requesting an EXPERIMENTAL_* feature is the program's acceptance of
    // wgpu's terms for them: they may still have bugs that are undefined
    // behaviour. Nothing else turns them on.
    let experimental_features =
        if requested_features.intersects(wgpu::Features::all_experimental_mask()) {
            unsafe { wgpu::ExperimentalFeatures::enabled() }
        } else {
            wgpu::ExperimentalFeatures::disabled()
        };
    let descriptor = wgpu::DeviceDescriptor {
        required_features: requested_features,
        required_limits: requested_limits,
        experimental_features,
        memory_hints: match memory_hints {
            Some(1) => wgpu::MemoryHints::MemoryUsage,
            _ => wgpu::MemoryHints::Performance,
        },
        ..Default::default()
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        match adapter.request_device(&descriptor).await {
            Ok((device, queue)) => {
                let errors: Arc<Mutex<VecDeque<String>>> = Arc::default();
                let reported = errors.clone();
                device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
                    reported.lock().unwrap().push_back(error_text(&error));
                }));
                let queue = QUEUES.lock().unwrap().put(queue);
                if queue == 0 {
                    completion
                        .get()
                        .reject(Text::new("queue resource table is full").value());
                    return;
                }
                let lost = diagnostics::watch(&device);
                let handle = DEVICES.lock().unwrap().put(DeviceEntry {
                    device,
                    queue,
                    errors,
                    lost,
                });
                if handle == 0 {
                    QUEUES.lock().unwrap().remove(queue);
                    completion
                        .get()
                        .reject(Text::new("device resource table is full").value());
                    return;
                }
                if !completion
                    .get()
                    .resolve_boxed(Box::new(crate::GpuDevice { handle }))
                {
                    DEVICES.lock().unwrap().remove(handle);
                    QUEUES.lock().unwrap().remove(queue);
                }
            }
            Err(error) => {
                completion
                    .get()
                    .reject(Text::new(&error.to_string()).value());
            }
        }
    });
    future
}

pub unsafe fn device_request(adapter: i32) -> Future<crate::GpuDevice> {
    device_request_configured(adapter, &[], &[], &[], &[], None)
}

pub unsafe fn device_request_with(
    adapter: i32,
    descriptor: &GpuDeviceDescriptor,
) -> Future<crate::GpuDevice> {
    device_request_configured(
        adapter,
        &descriptor.requiredFeatures,
        &descriptor.requiredLimits,
        &descriptor.requiredNativeFeatures,
        &descriptor.requiredNativeLimits,
        descriptor.memoryHints,
    )
}

pub unsafe fn device_take_error(device: i32) -> Text {
    let entry = find!(DEVICES, device, Text::NULL);
    let next = entry.errors.lock().unwrap().pop_front();
    match next {
        Some(message) => text_out(&message),
        None => Text::NULL,
    }
}

pub unsafe fn device_queue(device: i32) -> i32 {
    let entry = find!(DEVICES, device, 0);
    entry.queue
}

pub unsafe fn device_poll(device: i32) {
    let entry = find!(DEVICES, device);
    let _ = entry.device.poll(wgpu::PollType::Poll);
}

/// Destroys the device, as WebGPU's destroy does: its lost future resolves
/// with Destroyed once a poll sees the queue drained. wgpu reaches the queue
/// weakly, so it stays alive until that poll.
pub unsafe fn device_destroy(device: i32) {
    let Some(entry) = DEVICES.lock().unwrap().get(device) else {
        return;
    };
    DEVICES.lock().unwrap().remove(device);
    let queue = QUEUES.lock().unwrap().get(entry.queue);
    QUEUES.lock().unwrap().remove(entry.queue);
    entry.device.destroy();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let device = entry.device.clone();
        std::thread::spawn(move || {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            drop(queue);
        });
    }
    #[cfg(target_arch = "wasm32")]
    drop(queue);
}

// -- buffers ----------------------------------------------------------------

pub unsafe fn buffer_create(device: i32, descriptor: &GpuBufferDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    let buffer = entry.device.create_buffer(&wgpu::BufferDescriptor {
        label: label.as_ref().map(Text::as_str),
        size: descriptor.size.max(0) as u64,
        usage: wgpu::BufferUsages::from_bits_truncate(descriptor.usage as u32),
        mapped_at_creation: descriptor.mappedAtCreation.unwrap_or(false),
    });
    BUFFERS.lock().unwrap().put(buffer)
}

pub unsafe fn queue_write_buffer(queue: i32, buffer: i32, offset: i64, data: Buffer, len: i32) {
    let Some(bytes) = bytes(&data, len) else {
        return;
    };
    if bytes.is_empty() {
        return;
    }
    let queue = find!(QUEUES, queue);
    let buffer = find!(BUFFERS, buffer);
    queue.write_buffer(&buffer, offset.max(0) as u64, bytes);
}

pub unsafe fn buffer_map_begin(device: i32, buffer: i32, offset: i64, size: i64) -> Future<()> {
    unsafe { buffer_map_with(device, buffer, 1, offset, size) }
}

pub unsafe fn buffer_map_with(
    device: i32,
    buffer: i32,
    mode: i32,
    offset: i64,
    size: i64,
) -> Future<()> {
    let mode = match mode {
        1 => wgpu::MapMode::Read,
        2 => wgpu::MapMode::Write,
        _ => return rejected_future("a map mode is READ or WRITE"),
    };
    let Some(buffer) = BUFFERS.lock().unwrap().get(buffer) else {
        return rejected_future("buffer was destroyed");
    };
    let Some(device) = DEVICES.lock().unwrap().get(device) else {
        return rejected_future("device was destroyed");
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    let start = offset.max(0) as u64;
    buffer.map_async(
        mode,
        start..start + size.max(0) as u64,
        move |outcome| match outcome {
            Ok(()) => {
                completion.get().resolve(Value::null());
            }
            Err(error) => {
                completion
                    .get()
                    .reject(Text::new(&error.to_string()).value());
            }
        },
    );
    drive_device(device.device.clone());
    future
}

pub unsafe fn buffer_copy_out(buffer: i32, offset: i64, out: Buffer, len: i32) -> bool {
    if bytes(&out, len).is_none() || len <= 0 {
        return false;
    }
    let buffer = find!(BUFFERS, buffer, false);
    let start = offset.max(0) as u64;
    let Ok(view) = buffer.get_mapped_range(start..start + len as u64) else {
        return false;
    };
    unsafe {
        std::ptr::copy_nonoverlapping(view.as_ptr(), out.as_ptr() as *mut u8, len as usize);
    }
    true
}

pub unsafe fn buffer_unmap(buffer: i32) {
    let buffer = find!(BUFFERS, buffer);
    buffer.unmap();
}

pub unsafe fn buffer_destroy(buffer: i32) {
    BUFFERS.lock().unwrap().remove(buffer);
}

// -- shaders and pipelines --------------------------------------------------

pub unsafe fn shader_create(device: i32, wgsl: Text) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let source = wgsl.as_str();
    let module = entry
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    SHADERS.lock().unwrap().put(module)
}

pub unsafe fn shader_destroy(shader: i32) {
    SHADERS.lock().unwrap().remove(shader);
}

pub unsafe fn compute_pipeline_create(device: i32, shader: i32, entry: Text) -> i32 {
    let device_entry = find!(DEVICES, device, 0);
    let module = find!(SHADERS, shader, 0);
    let name = entry.as_str();
    let pipeline = device_entry
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            // Inferred from the shader, so a bind group only needs buffers.
            layout: None,
            module: &module,
            entry_point: Some(name),
            compilation_options: Default::default(),
            cache: None,
        });
    PIPELINES.lock().unwrap().put(pipeline)
}

pub unsafe fn pipeline_destroy(pipeline: i32) {
    PIPELINES.lock().unwrap().remove(pipeline);
}

/// One entry of a bind group, held so the descriptor below can borrow it.
#[derive(Clone)]
enum Bound {
    Buffer(Arc<wgpu::Buffer>),
    View(Arc<wgpu::TextureView>),
    Sampler(Arc<wgpu::Sampler>),
}

pub unsafe fn bind_group_create(device: i32, pipeline: i32, group: i32, bindings: i32) -> i32 {
    let bindings = find!(BINDINGS, bindings, 0);
    let found = bindings.lock().unwrap().clone();
    let entry = find!(DEVICES, device, 0);
    let group = group.max(0) as u32;

    // A compute or a render pipeline; both have layouts, and the handle says
    // which it is.
    let layout = if kind_of(pipeline) == Kind::Renderpipeline as i32 {
        find!(RENDER_PIPELINES, pipeline, 0).get_bind_group_layout(group)
    } else {
        find!(PIPELINES, pipeline, 0).get_bind_group_layout(group)
    };

    let entries: Vec<wgpu::BindGroupEntry> = found
        .iter()
        .enumerate()
        .map(|(binding, one)| wgpu::BindGroupEntry {
            binding: binding as u32,
            resource: match one {
                Bound::Buffer(buffer) => buffer.as_entire_binding(),
                Bound::View(view) => wgpu::BindingResource::TextureView(view),
                Bound::Sampler(sampler) => wgpu::BindingResource::Sampler(sampler),
            },
        })
        .collect();

    let bind_group = entry.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &entries,
    });
    BINDGROUPS.lock().unwrap().put(bind_group)
}

pub unsafe fn bind_group_destroy(bindgroup: i32) {
    BINDGROUPS.lock().unwrap().remove(bindgroup);
}

// -- explicit layouts -----------------------------------------------------------

slab!(
    BIND_GROUP_LAYOUTS,
    wgpu::BindGroupLayout,
    Kind::BindGroupLayout
);
slab!(PIPELINE_LAYOUTS, wgpu::PipelineLayout, Kind::PipelineLayout);
slab!(QUERY_SETS, QuerySetEntry, Kind::QuerySet);
slab!(
    BUNDLE_ENCODERS,
    Mutex<bundles::Recording>,
    Kind::BundleEncoder
);
slab!(BUNDLES, wgpu::RenderBundle, Kind::Bundle);
slab!(ERRORS, diagnostics::ErrorEntry, Kind::Error);
slab!(LOST_INFOS, diagnostics::LostEntry, Kind::LostInfo);
slab!(
    COMPILATIONS,
    Vec<diagnostics::Message>,
    Kind::CompilationInfo
);
slab!(
    CAPABILITIES,
    surfaces::Capabilities,
    Kind::SurfaceCapabilities
);
slab!(
    EXTERNAL_TEXTURES,
    wgpu::ExternalTexture,
    Kind::ExternalTexture
);
slab!(BLASES, ray_tracing::BlasEntry, Kind::Blas);
// A TLAS's instances are set through `&mut`, so it sits behind a lock.
slab!(TLASES, Mutex<wgpu::Tlas>, Kind::Tlas);

/// A descriptor the caller got wrong: raised in the caller's language, with
/// no GPU work done.
fn refuse(message: &str) -> i32 {
    host::raise(ErrorKind::Type, message);
    0
}

fn index(value: i32, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} {value} is negative"))
}

fn size(value: i64, what: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{what} {value} is negative"))
}

// The orders below are the WebGPU IDL's, which the generated enums carry.
fn buffer_binding_type(which: i32) -> wgpu::BufferBindingType {
    match which {
        1 => wgpu::BufferBindingType::Storage { read_only: false },
        2 => wgpu::BufferBindingType::Storage { read_only: true },
        _ => wgpu::BufferBindingType::Uniform,
    }
}

fn sampler_binding_type(which: i32) -> wgpu::SamplerBindingType {
    match which {
        1 => wgpu::SamplerBindingType::NonFiltering,
        2 => wgpu::SamplerBindingType::Comparison,
        _ => wgpu::SamplerBindingType::Filtering,
    }
}

fn texture_sample_type(which: i32) -> wgpu::TextureSampleType {
    match which {
        1 => wgpu::TextureSampleType::Float { filterable: false },
        2 => wgpu::TextureSampleType::Depth,
        3 => wgpu::TextureSampleType::Sint,
        4 => wgpu::TextureSampleType::Uint,
        _ => wgpu::TextureSampleType::Float { filterable: true },
    }
}

fn storage_texture_access(which: i32) -> wgpu::StorageTextureAccess {
    match which {
        1 => wgpu::StorageTextureAccess::ReadOnly,
        2 => wgpu::StorageTextureAccess::ReadWrite,
        _ => wgpu::StorageTextureAccess::WriteOnly,
    }
}

/// What one layout entry binds: exactly one of its five layouts, each with
/// the IDL's defaults for what was left unset.
fn binding_type(entry: &GpuBindGroupLayoutEntry) -> Result<wgpu::BindingType, String> {
    let mut chosen = Vec::with_capacity(1);
    if let Some(buffer) = &entry.buffer {
        let min = size(buffer.minBindingSize.unwrap_or(0), "minBindingSize")?;
        chosen.push(wgpu::BindingType::Buffer {
            ty: buffer_binding_type(buffer.r#type.unwrap_or(0)),
            has_dynamic_offset: buffer.hasDynamicOffset.unwrap_or(false),
            min_binding_size: std::num::NonZeroU64::new(min),
        });
    }
    if let Some(sampler) = &entry.sampler {
        chosen.push(wgpu::BindingType::Sampler(sampler_binding_type(
            sampler.r#type.unwrap_or(0),
        )));
    }
    if let Some(texture) = &entry.texture {
        chosen.push(wgpu::BindingType::Texture {
            sample_type: texture_sample_type(texture.sampleType.unwrap_or(0)),
            view_dimension: texture_view_dimension(texture.viewDimension.unwrap_or(1)),
            multisampled: texture.multisampled.unwrap_or(false),
        });
    }
    if let Some(storage) = &entry.storageTexture {
        chosen.push(wgpu::BindingType::StorageTexture {
            access: storage_texture_access(storage.access.unwrap_or(0)),
            format: texture_format(storage.format),
            view_dimension: texture_view_dimension(storage.viewDimension.unwrap_or(1)),
        });
    }
    if entry.externalTexture.is_some() {
        chosen.push(wgpu::BindingType::ExternalTexture);
    }
    if let Some(structure) = &entry.accelerationStructure {
        chosen.push(wgpu::BindingType::AccelerationStructure {
            vertex_return: structure.vertexReturn.unwrap_or(false),
        });
    }
    match (chosen.pop(), chosen.is_empty()) {
        (Some(ty), true) => Ok(ty),
        _ => Err(format!(
            "binding {} needs exactly one of buffer, sampler, texture, storageTexture, externalTexture or accelerationStructure",
            entry.binding
        )),
    }
}

pub unsafe fn bind_group_layout_create(
    device: i32,
    descriptor: &GpuBindGroupLayoutDescriptor,
) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let entries: Result<Vec<_>, String> = descriptor
        .entries
        .iter()
        .map(|entry| {
            Ok(wgpu::BindGroupLayoutEntry {
                binding: index(entry.binding, "binding")?,
                visibility: wgpu::ShaderStages::from_bits_truncate(entry.visibility as u32),
                ty: binding_type(entry)?,
                count: match entry.count {
                    None => None,
                    Some(count) => Some(
                        std::num::NonZeroU32::new(index(count, "count")?)
                            .ok_or("a binding array of 0 elements")?,
                    ),
                },
            })
        })
        .collect();
    let entries = match entries {
        Ok(entries) => entries,
        Err(message) => return refuse(&message),
    };
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    let layout = entry
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: label.as_ref().map(Text::as_str),
            entries: &entries,
        });
    BIND_GROUP_LAYOUTS.lock().unwrap().put(layout)
}

pub unsafe fn bind_group_layout_destroy(layout: i32) {
    BIND_GROUP_LAYOUTS.lock().unwrap().remove(layout);
}

pub unsafe fn pipeline_layout_create(device: i32, descriptor: &GpuPipelineLayoutDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    // Resolved first so the descriptor below can borrow them. A null slot
    // is a group the pipeline leaves empty.
    let mut layouts = Vec::with_capacity(descriptor.bindGroupLayouts.len());
    for (group, handle) in descriptor.bindGroupLayouts.iter().enumerate() {
        match handle {
            Some(handle) => match BIND_GROUP_LAYOUTS.lock().unwrap().get(*handle) {
                Some(layout) => layouts.push(Some(layout)),
                None => return refuse(&format!("bind group layout {group} was destroyed")),
            },
            None => layouts.push(None),
        }
    }
    let immediate_size = match index(descriptor.immediateSize.unwrap_or(0), "immediateSize") {
        Ok(size) => size,
        Err(message) => return refuse(&message),
    };
    let borrowed: Vec<Option<&wgpu::BindGroupLayout>> =
        layouts.iter().map(|layout| layout.as_deref()).collect();
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    let layout = entry
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: label.as_ref().map(Text::as_str),
            bind_group_layouts: &borrowed,
            immediate_size,
        });
    PIPELINE_LAYOUTS.lock().unwrap().put(layout)
}

pub unsafe fn pipeline_layout_destroy(layout: i32) {
    PIPELINE_LAYOUTS.lock().unwrap().remove(layout);
}

/// One resolved bind group resource, held so the descriptor can borrow it.
/// A TLAS is an index into the bind group's distinct locked TLASes.
enum Resolved {
    Buffer(Arc<wgpu::Buffer>, u64, Option<std::num::NonZeroU64>),
    Sampler(Arc<wgpu::Sampler>),
    View(Arc<wgpu::TextureView>),
    External(Arc<wgpu::ExternalTexture>),
    Buffers(Vec<(Arc<wgpu::Buffer>, u64, Option<std::num::NonZeroU64>)>),
    Samplers(Vec<Arc<wgpu::Sampler>>),
    Views(Vec<Arc<wgpu::TextureView>>),
    Tlas(usize),
    Tlases(Vec<usize>),
}

/// The TLASes a bind group names, each once, so each is locked once.
#[derive(Default)]
struct Distinct(Vec<Arc<Mutex<wgpu::Tlas>>>);

impl Distinct {
    fn slot(&mut self, handle: i32) -> Result<usize, String> {
        let tlas = TLASES
            .lock()
            .unwrap()
            .get(handle)
            .ok_or("the TLAS was destroyed")?;
        Ok(
            match self.0.iter().position(|seen| Arc::ptr_eq(seen, &tlas)) {
                Some(at) => at,
                None => {
                    self.0.push(tlas);
                    self.0.len() - 1
                }
            },
        )
    }
}

fn buffer_range(
    binding: i32,
    range: &crate::GpuBufferBinding,
) -> Result<(Arc<wgpu::Buffer>, u64, Option<std::num::NonZeroU64>), String> {
    let buffer = BUFFERS
        .lock()
        .unwrap()
        .get(range.buffer)
        .ok_or_else(|| format!("binding {binding}: the buffer was destroyed"))?;
    let offset = size(range.offset.unwrap_or(0), "offset")?;
    let length = match range.size {
        Some(length) => Some(
            std::num::NonZeroU64::new(size(length, "size")?)
                .ok_or_else(|| format!("binding {binding}: a buffer range of size 0"))?,
        ),
        None => None,
    };
    Ok((buffer, offset, length))
}

fn resolve(
    binding: i32,
    resource: Option<&BindingResource>,
    tlases: &mut Distinct,
) -> Result<Resolved, String> {
    let gone = |what: &str| format!("binding {binding}: the {what} was destroyed");
    let sampler = |handle: i32| {
        SAMPLERS
            .lock()
            .unwrap()
            .get(handle)
            .ok_or_else(|| gone("sampler"))
    };
    let view = |handle: i32| {
        VIEWS
            .lock()
            .unwrap()
            .get(handle)
            .ok_or_else(|| gone("texture view"))
    };
    Ok(match resource {
        None => return Err(format!("binding {binding} has no resource")),
        Some(BindingResource::Sampler(handle)) => Resolved::Sampler(sampler(*handle)?),
        Some(BindingResource::TextureView(handle)) => Resolved::View(view(*handle)?),
        // A texture binds its default view, as WebGPU specifies.
        Some(BindingResource::Texture(handle)) => {
            let texture = TEXTURES
                .lock()
                .unwrap()
                .get(*handle)
                .ok_or_else(|| gone("texture"))?;
            Resolved::View(Arc::new(texture.create_view(&Default::default())))
        }
        Some(BindingResource::Buffer(handle)) => Resolved::Buffer(
            BUFFERS
                .lock()
                .unwrap()
                .get(*handle)
                .ok_or_else(|| gone("buffer"))?,
            0,
            None,
        ),
        Some(BindingResource::BufferBinding(range)) => {
            let (buffer, offset, length) = buffer_range(binding, range)?;
            Resolved::Buffer(buffer, offset, length)
        }
        Some(BindingResource::ExternalTexture(handle)) => Resolved::External(
            EXTERNAL_TEXTURES
                .lock()
                .unwrap()
                .get(*handle)
                .ok_or_else(|| gone("external texture"))?,
        ),
        Some(BindingResource::BufferArray(array)) => Resolved::Buffers(
            array
                .buffers
                .iter()
                .map(|range| buffer_range(binding, range))
                .collect::<Result<_, _>>()?,
        ),
        Some(BindingResource::SamplerArray(array)) => Resolved::Samplers(
            array
                .samplers
                .iter()
                .map(|&handle| sampler(handle))
                .collect::<Result<_, _>>()?,
        ),
        Some(BindingResource::TextureViewArray(array)) => Resolved::Views(
            array
                .views
                .iter()
                .map(|&handle| view(handle))
                .collect::<Result<_, _>>()?,
        ),
        Some(BindingResource::AccelerationStructure(handle)) => {
            Resolved::Tlas(tlases.slot(*handle)?)
        }
        Some(BindingResource::AccelerationStructureArray(array)) => Resolved::Tlases(
            array
                .tlases
                .iter()
                .map(|&handle| tlases.slot(handle))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// A resolved array as the slice wgpu borrows.
enum Lent<'a> {
    None,
    Buffers(Vec<wgpu::BufferBinding<'a>>),
    Samplers(Vec<&'a wgpu::Sampler>),
    Views(Vec<&'a wgpu::TextureView>),
    Tlases(Vec<&'a wgpu::Tlas>),
}

pub unsafe fn bind_group_create_with(device: i32, descriptor: &GpuBindGroupDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let Some(layout) = BIND_GROUP_LAYOUTS.lock().unwrap().get(descriptor.layout) else {
        return refuse("the bind group layout was destroyed");
    };
    let mut tlases = Distinct::default();
    let mut resolved = Vec::with_capacity(descriptor.entries.len());
    for one in &descriptor.entries {
        let binding = match index(one.binding, "binding") {
            Ok(binding) => binding,
            Err(message) => return refuse(&message),
        };
        match resolve(one.binding, one.resource.as_ref(), &mut tlases) {
            Ok(resource) => resolved.push((binding, resource)),
            Err(message) => return refuse(&message),
        }
    }
    let locked: Vec<_> = tlases.0.iter().map(|tlas| tlas.lock().unwrap()).collect();
    let lent: Vec<Lent> = resolved
        .iter()
        .map(|(_, resource)| match resource {
            Resolved::Buffers(list) => Lent::Buffers(
                list.iter()
                    .map(|(buffer, offset, size)| wgpu::BufferBinding {
                        buffer,
                        offset: *offset,
                        size: *size,
                    })
                    .collect(),
            ),
            Resolved::Samplers(list) => Lent::Samplers(list.iter().map(|s| &**s).collect()),
            Resolved::Views(list) => Lent::Views(list.iter().map(|v| &**v).collect()),
            Resolved::Tlases(list) => Lent::Tlases(list.iter().map(|&at| &*locked[at]).collect()),
            _ => Lent::None,
        })
        .collect();
    let entries: Vec<wgpu::BindGroupEntry> = resolved
        .iter()
        .zip(&lent)
        .map(|((binding, resource), lent)| wgpu::BindGroupEntry {
            binding: *binding,
            resource: match (resource, lent) {
                (Resolved::Buffer(buffer, offset, size), _) => {
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: *offset,
                        size: *size,
                    })
                }
                (Resolved::Sampler(sampler), _) => wgpu::BindingResource::Sampler(sampler),
                (Resolved::View(view), _) => wgpu::BindingResource::TextureView(view),
                (Resolved::External(texture), _) => wgpu::BindingResource::ExternalTexture(texture),
                (Resolved::Tlas(at), _) => {
                    wgpu::BindingResource::AccelerationStructure(&locked[*at])
                }
                (_, Lent::Buffers(list)) => wgpu::BindingResource::BufferArray(list),
                (_, Lent::Samplers(list)) => wgpu::BindingResource::SamplerArray(list),
                (_, Lent::Views(list)) => wgpu::BindingResource::TextureViewArray(list),
                (_, Lent::Tlases(list)) => wgpu::BindingResource::AccelerationStructureArray(list),
                (_, Lent::None) => unreachable!("every array resolves to a lent slice"),
            },
        })
        .collect();
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    let group = entry.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: label.as_ref().map(Text::as_str),
        layout: &layout,
        entries: &entries,
    });
    BINDGROUPS.lock().unwrap().put(group)
}

/// A pipeline's layout for group `index`, inferred or explicit. The layout
/// outlives the pipeline and can build bind groups for other pipelines
/// that share it.
pub unsafe fn pipeline_bind_group_layout(pipeline: i32, index_of: i32) -> i32 {
    let group = match index(index_of, "bind group index") {
        Ok(group) => group,
        Err(message) => return refuse(&message),
    };
    let layout = if kind_of(pipeline) == Kind::Renderpipeline as i32 {
        find!(RENDER_PIPELINES, pipeline, 0).get_bind_group_layout(group)
    } else {
        find!(PIPELINES, pipeline, 0).get_bind_group_layout(group)
    };
    BIND_GROUP_LAYOUTS.lock().unwrap().put(layout)
}

// -- dynamic offsets and compute passes ------------------------------------------

/// `count` dynamic offsets from element `start` of a shared buffer of
/// 32-bit values, as WebGPU's Uint32Array overload of setBindGroup reads
/// them. Checked against the buffer before anything is read.
fn dynamic_offsets(data: &Buffer, start: i64, count: i32) -> Option<Vec<u32>> {
    let (Ok(start), Ok(count)) = (usize::try_from(start), usize::try_from(count)) else {
        host::raise(ErrorKind::Type, "negative dynamic offset range");
        return None;
    };
    let end = start.checked_add(count).and_then(|end| end.checked_mul(4));
    if end.is_none_or(|end| end > data.len()) {
        host::raise(ErrorKind::Type, "dynamic offsets exceed the shared buffer");
        return None;
    }
    let bytes = unsafe { &data.as_slice()[start * 4..(start + count) * 4] };
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_ne_bytes(*word))
            .collect(),
    )
}

pub unsafe fn render_set_bind_group_offsets(
    encoder: i32,
    group: i32,
    bindgroup: i32,
    offsets: Buffer,
    start: i64,
    count: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let bind_group = find!(BINDGROUPS, bindgroup);
    let Some(offsets) = dynamic_offsets(&offsets, start, count) else {
        return;
    };
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_bind_group(group.max(0) as u32, &*bind_group, &offsets);
    }
}

pub unsafe fn compute_begin(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if held.pass.is_some() || held.compute.is_some() {
        host::raise(ErrorKind::Runtime, "a pass is already open on this encoder");
        return;
    }
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    let pass = encoder
        .begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        })
        .forget_lifetime();
    held.compute = Some(pass);
}

/// Runs `body` on the encoder's open compute pass, or raises.
fn computing(encoder: &Encoder, body: impl FnOnce(&mut wgpu::ComputePass<'static>)) {
    let mut held = encoder.lock().unwrap();
    match held.compute.as_mut() {
        Some(pass) => body(pass),
        None => host::raise(
            ErrorKind::Runtime,
            "no compute pass is open on this encoder",
        ),
    }
}

pub unsafe fn compute_set_pipeline(encoder: i32, pipeline: i32) {
    let entry = find!(ENCODERS, encoder);
    let pipeline = find!(PIPELINES, pipeline);
    computing(&entry, |pass| pass.set_pipeline(&pipeline));
}

pub unsafe fn compute_set_bind_group(encoder: i32, group: i32, bindgroup: i32) {
    let entry = find!(ENCODERS, encoder);
    let bind_group = find!(BINDGROUPS, bindgroup);
    computing(&entry, |pass| {
        pass.set_bind_group(group.max(0) as u32, &*bind_group, &[])
    });
}

pub unsafe fn compute_set_bind_group_offsets(
    encoder: i32,
    group: i32,
    bindgroup: i32,
    offsets: Buffer,
    start: i64,
    count: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let bind_group = find!(BINDGROUPS, bindgroup);
    let Some(offsets) = dynamic_offsets(&offsets, start, count) else {
        return;
    };
    computing(&entry, |pass| {
        pass.set_bind_group(group.max(0) as u32, &*bind_group, &offsets)
    });
}

pub unsafe fn compute_dispatch(encoder: i32, x: i32, y: i32, z: i32) {
    let entry = find!(ENCODERS, encoder);
    computing(&entry, |pass| {
        pass.dispatch_workgroups(x.max(0) as u32, y.max(0) as u32, z.max(0) as u32)
    });
}

pub unsafe fn compute_dispatch_indirect(encoder: i32, buffer: i32, offset: i64) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    computing(&entry, |pass| {
        pass.dispatch_workgroups_indirect(&buffer, offset.max(0) as u64)
    });
}

pub unsafe fn compute_end(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    // Dropping the pass is what ends it.
    entry.lock().unwrap().compute = None;
}

// -- commands ---------------------------------------------------------------

pub unsafe fn encoder_create(device: i32) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let encoder = entry
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    let mut held = EncoderEntry::default();
    held.encoder = Some(encoder);
    ENCODERS.lock().unwrap().put(Mutex::new(held))
}

pub unsafe fn encoder_compute(encoder: i32, pipeline: i32, bindgroup: i32, x: i32, y: i32, z: i32) {
    let encoder = find!(ENCODERS, encoder);
    let pipeline = find!(PIPELINES, pipeline);
    let bind_group = find!(BINDGROUPS, bindgroup);
    let mut held = encoder.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };

    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: None,
        timestamp_writes: None,
    });
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &*bind_group, &[]);
    pass.dispatch_workgroups(x.max(0) as u32, y.max(0) as u32, z.max(0) as u32);
}

pub unsafe fn encoder_copy_buffer(
    encoder: i32,
    src: i32,
    src_offset: i64,
    dst: i32,
    dst_offset: i64,
    size: i64,
) {
    let encoder = find!(ENCODERS, encoder);
    let source = find!(BUFFERS, src);
    let target = find!(BUFFERS, dst);
    let mut held = encoder.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    encoder.copy_buffer_to_buffer(
        &source,
        src_offset.max(0) as u64,
        &target,
        dst_offset.max(0) as u64,
        size.max(0) as u64,
    );
}

pub unsafe fn encoder_submit(encoder: i32, queue: i32) {
    let handle = encoder;
    let encoder = find!(ENCODERS, handle);
    let queue = find!(QUEUES, queue);
    let taken = encoder.lock().unwrap().encoder.take();
    if let Some(encoder) = taken {
        queue.submit([encoder.finish()]);
    }
    // Spent either way: an encoder cannot be finished twice.
    ENCODERS.lock().unwrap().remove(handle);
}

pub unsafe fn queue_work_done(device: i32, queue: i32) -> Future<()> {
    let Some(queue) = QUEUES.lock().unwrap().get(queue) else {
        return rejected_future("queue was destroyed");
    };
    let Some(device) = DEVICES.lock().unwrap().get(device) else {
        return rejected_future("device was destroyed");
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    queue.on_submitted_work_done(move || {
        completion.get().resolve(Value::null());
    });
    drive_device(device.device.clone());
    future
}

// -- textures ---------------------------------------------------------------

fn texture_format(which: i32) -> wgpu::TextureFormat {
    use wgpu::{AstcBlock as B, AstcChannel as C, TextureFormat as F};
    match which {
        0 => F::R8Unorm,
        1 => F::R8Snorm,
        2 => F::R8Uint,
        3 => F::R8Sint,
        4 => F::R16Unorm,
        5 => F::R16Snorm,
        6 => F::R16Uint,
        7 => F::R16Sint,
        8 => F::R16Float,
        9 => F::Rg8Unorm,
        10 => F::Rg8Snorm,
        11 => F::Rg8Uint,
        12 => F::Rg8Sint,
        13 => F::R32Uint,
        14 => F::R32Sint,
        15 => F::R32Float,
        16 => F::Rg16Unorm,
        17 => F::Rg16Snorm,
        18 => F::Rg16Uint,
        19 => F::Rg16Sint,
        20 => F::Rg16Float,
        21 => F::Rgba8Unorm,
        22 => F::Rgba8UnormSrgb,
        23 => F::Rgba8Snorm,
        24 => F::Rgba8Uint,
        25 => F::Rgba8Sint,
        26 => F::Bgra8Unorm,
        27 => F::Bgra8UnormSrgb,
        28 => F::Rgb9e5Ufloat,
        29 => F::Rgb10a2Uint,
        30 => F::Rgb10a2Unorm,
        31 => F::Rg11b10Ufloat,
        32 => F::Rg32Uint,
        33 => F::Rg32Sint,
        34 => F::Rg32Float,
        35 => F::Rgba16Unorm,
        36 => F::Rgba16Snorm,
        37 => F::Rgba16Uint,
        38 => F::Rgba16Sint,
        39 => F::Rgba16Float,
        40 => F::Rgba32Uint,
        41 => F::Rgba32Sint,
        42 => F::Rgba32Float,
        43 => F::Stencil8,
        44 => F::Depth16Unorm,
        45 => F::Depth24Plus,
        46 => F::Depth24PlusStencil8,
        47 => F::Depth32Float,
        48 => F::Depth32FloatStencil8,
        49 => F::Bc1RgbaUnorm,
        50 => F::Bc1RgbaUnormSrgb,
        51 => F::Bc2RgbaUnorm,
        52 => F::Bc2RgbaUnormSrgb,
        53 => F::Bc3RgbaUnorm,
        54 => F::Bc3RgbaUnormSrgb,
        55 => F::Bc4RUnorm,
        56 => F::Bc4RSnorm,
        57 => F::Bc5RgUnorm,
        58 => F::Bc5RgSnorm,
        59 => F::Bc6hRgbUfloat,
        60 => F::Bc6hRgbFloat,
        61 => F::Bc7RgbaUnorm,
        62 => F::Bc7RgbaUnormSrgb,
        63 => F::Etc2Rgb8Unorm,
        64 => F::Etc2Rgb8UnormSrgb,
        65 => F::Etc2Rgb8A1Unorm,
        66 => F::Etc2Rgb8A1UnormSrgb,
        67 => F::Etc2Rgba8Unorm,
        68 => F::Etc2Rgba8UnormSrgb,
        69 => F::EacR11Unorm,
        70 => F::EacR11Snorm,
        71 => F::EacRg11Unorm,
        72 => F::EacRg11Snorm,
        73 => F::Astc {
            block: B::B4x4,
            channel: C::Unorm,
        },
        74 => F::Astc {
            block: B::B4x4,
            channel: C::UnormSrgb,
        },
        75 => F::Astc {
            block: B::B5x4,
            channel: C::Unorm,
        },
        76 => F::Astc {
            block: B::B5x4,
            channel: C::UnormSrgb,
        },
        77 => F::Astc {
            block: B::B5x5,
            channel: C::Unorm,
        },
        78 => F::Astc {
            block: B::B5x5,
            channel: C::UnormSrgb,
        },
        79 => F::Astc {
            block: B::B6x5,
            channel: C::Unorm,
        },
        80 => F::Astc {
            block: B::B6x5,
            channel: C::UnormSrgb,
        },
        81 => F::Astc {
            block: B::B6x6,
            channel: C::Unorm,
        },
        82 => F::Astc {
            block: B::B6x6,
            channel: C::UnormSrgb,
        },
        83 => F::Astc {
            block: B::B8x5,
            channel: C::Unorm,
        },
        84 => F::Astc {
            block: B::B8x5,
            channel: C::UnormSrgb,
        },
        85 => F::Astc {
            block: B::B8x6,
            channel: C::Unorm,
        },
        86 => F::Astc {
            block: B::B8x6,
            channel: C::UnormSrgb,
        },
        87 => F::Astc {
            block: B::B8x8,
            channel: C::Unorm,
        },
        88 => F::Astc {
            block: B::B8x8,
            channel: C::UnormSrgb,
        },
        89 => F::Astc {
            block: B::B10x5,
            channel: C::Unorm,
        },
        90 => F::Astc {
            block: B::B10x5,
            channel: C::UnormSrgb,
        },
        91 => F::Astc {
            block: B::B10x6,
            channel: C::Unorm,
        },
        92 => F::Astc {
            block: B::B10x6,
            channel: C::UnormSrgb,
        },
        93 => F::Astc {
            block: B::B10x8,
            channel: C::Unorm,
        },
        94 => F::Astc {
            block: B::B10x8,
            channel: C::UnormSrgb,
        },
        95 => F::Astc {
            block: B::B10x10,
            channel: C::Unorm,
        },
        96 => F::Astc {
            block: B::B10x10,
            channel: C::UnormSrgb,
        },
        97 => F::Astc {
            block: B::B12x10,
            channel: C::Unorm,
        },
        98 => F::Astc {
            block: B::B12x10,
            channel: C::UnormSrgb,
        },
        99 => F::Astc {
            block: B::B12x12,
            channel: C::Unorm,
        },
        100 => F::Astc {
            block: B::B12x12,
            channel: C::UnormSrgb,
        },
        // wgpu's own, after the WebGPU list.
        101 => F::R64Uint,
        102 => F::NV12,
        103 => F::P010,
        _ => panic!("unknown texture format"),
    }
}

/// The highest texture format code.
const LAST_TEXTURE_FORMAT: i32 = 103;

/// The code of a wgpu format, for what the backend reports back.
fn texture_format_code(format: wgpu::TextureFormat) -> Option<i32> {
    (0..=LAST_TEXTURE_FORMAT).find(|&code| texture_format(code) == format)
}

fn vertex_format(which: i32) -> wgpu::VertexFormat {
    match which {
        0 => wgpu::VertexFormat::Uint8,
        1 => wgpu::VertexFormat::Uint8x2,
        2 => wgpu::VertexFormat::Uint8x4,
        3 => wgpu::VertexFormat::Sint8,
        4 => wgpu::VertexFormat::Sint8x2,
        5 => wgpu::VertexFormat::Sint8x4,
        6 => wgpu::VertexFormat::Unorm8,
        7 => wgpu::VertexFormat::Unorm8x2,
        8 => wgpu::VertexFormat::Unorm8x4,
        9 => wgpu::VertexFormat::Snorm8,
        10 => wgpu::VertexFormat::Snorm8x2,
        11 => wgpu::VertexFormat::Snorm8x4,
        12 => wgpu::VertexFormat::Uint16,
        13 => wgpu::VertexFormat::Uint16x2,
        14 => wgpu::VertexFormat::Uint16x4,
        15 => wgpu::VertexFormat::Sint16,
        16 => wgpu::VertexFormat::Sint16x2,
        17 => wgpu::VertexFormat::Sint16x4,
        18 => wgpu::VertexFormat::Unorm16,
        19 => wgpu::VertexFormat::Unorm16x2,
        20 => wgpu::VertexFormat::Unorm16x4,
        21 => wgpu::VertexFormat::Snorm16,
        22 => wgpu::VertexFormat::Snorm16x2,
        23 => wgpu::VertexFormat::Snorm16x4,
        24 => wgpu::VertexFormat::Float16,
        25 => wgpu::VertexFormat::Float16x2,
        26 => wgpu::VertexFormat::Float16x4,
        27 => wgpu::VertexFormat::Float32,
        28 => wgpu::VertexFormat::Float32x2,
        29 => wgpu::VertexFormat::Float32x3,
        30 => wgpu::VertexFormat::Float32x4,
        31 => wgpu::VertexFormat::Uint32,
        32 => wgpu::VertexFormat::Uint32x2,
        33 => wgpu::VertexFormat::Uint32x3,
        34 => wgpu::VertexFormat::Uint32x4,
        35 => wgpu::VertexFormat::Sint32,
        36 => wgpu::VertexFormat::Sint32x2,
        37 => wgpu::VertexFormat::Sint32x3,
        38 => wgpu::VertexFormat::Sint32x4,
        39 => wgpu::VertexFormat::Unorm10_10_10_2,
        40 => wgpu::VertexFormat::Unorm8x4Bgra,
        41 => panic!("snorm10-10-10-2 is unavailable in wgpu 30"),
        // wgpu's own, after the WebGPU list.
        42 => wgpu::VertexFormat::Float64,
        43 => wgpu::VertexFormat::Float64x2,
        44 => wgpu::VertexFormat::Float64x3,
        45 => wgpu::VertexFormat::Float64x4,
        _ => panic!("unknown vertex format"),
    }
}

fn texture_dimension(value: i32) -> wgpu::TextureDimension {
    match value {
        0 => wgpu::TextureDimension::D1,
        2 => wgpu::TextureDimension::D3,
        _ => wgpu::TextureDimension::D2,
    }
}

pub unsafe fn texture_create(device: i32, descriptor: &GpuTextureDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    assert!(
        descriptor.textureBindingViewDimension.is_none(),
        "texture binding view dimensions are not supported by this wgpu version"
    );
    let view_formats: Vec<_> = descriptor
        .viewFormats
        .iter()
        .copied()
        .map(texture_format)
        .collect();
    let texture = entry.device.create_texture(&wgpu::TextureDescriptor {
        label: label.as_ref().map(Text::as_str),
        size: wgpu::Extent3d {
            width: descriptor.size.width.max(1) as u32,
            height: descriptor.size.height.unwrap_or(1).max(1) as u32,
            depth_or_array_layers: descriptor.size.depthOrArrayLayers.unwrap_or(1).max(1) as u32,
        },
        mip_level_count: descriptor.mipLevelCount.unwrap_or(1).max(1) as u32,
        sample_count: descriptor.sampleCount.unwrap_or(1).max(1) as u32,
        dimension: texture_dimension(descriptor.dimension.unwrap_or(1)),
        format: texture_format(descriptor.format),
        usage: wgpu::TextureUsages::from_bits_truncate(descriptor.usage as u32),
        view_formats: &view_formats,
    });
    TEXTURES.lock().unwrap().put(texture)
}

fn texture_view_dimension(value: i32) -> wgpu::TextureViewDimension {
    match value {
        0 => wgpu::TextureViewDimension::D1,
        1 => wgpu::TextureViewDimension::D2,
        2 => wgpu::TextureViewDimension::D2Array,
        3 => wgpu::TextureViewDimension::Cube,
        4 => wgpu::TextureViewDimension::CubeArray,
        5 => wgpu::TextureViewDimension::D3,
        _ => panic!("unsupported texture view dimension"),
    }
}

fn texture_aspect(value: i32) -> wgpu::TextureAspect {
    match value {
        1 => wgpu::TextureAspect::StencilOnly,
        2 => wgpu::TextureAspect::DepthOnly,
        _ => wgpu::TextureAspect::All,
    }
}

pub unsafe fn texture_view(texture: i32, descriptor: &GpuTextureViewDescriptor) -> i32 {
    let texture = find!(TEXTURES, texture, 0);
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    if let Some(swizzle) = descriptor.swizzle.as_ref().map(caribou_abi::Rooted::get) {
        assert_eq!(
            swizzle.as_str(),
            "rgba",
            "texture component swizzle is not supported by this wgpu version"
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: label.as_ref().map(Text::as_str),
        format: descriptor.format.map(texture_format),
        dimension: descriptor.dimension.map(texture_view_dimension),
        usage: descriptor
            .usage
            .filter(|usage| *usage != 0)
            .map(|usage| wgpu::TextureUsages::from_bits_truncate(usage as u32)),
        aspect: texture_aspect(descriptor.aspect.unwrap_or(0)),
        base_mip_level: descriptor.baseMipLevel.unwrap_or(0).max(0) as u32,
        mip_level_count: descriptor.mipLevelCount.map(|count| count.max(0) as u32),
        base_array_layer: descriptor.baseArrayLayer.unwrap_or(0).max(0) as u32,
        array_layer_count: descriptor.arrayLayerCount.map(|count| count.max(0) as u32),
    });
    VIEWS.lock().unwrap().put(view)
}

pub unsafe fn texture_destroy(texture: i32) {
    TEXTURES.lock().unwrap().remove(texture);
}

pub unsafe fn view_destroy(view: i32) {
    VIEWS.lock().unwrap().remove(view);
}

// -- render pipelines -------------------------------------------------------

pub unsafe fn render_pipeline_destroy(pipeline: i32) {
    RENDER_PIPELINES.lock().unwrap().remove(pipeline);
}

// -- render passes ----------------------------------------------------------

pub unsafe fn pass_reset(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    held.colour.clear();
    held.depth = None;
}

pub unsafe fn pass_colour(encoder: i32, view: i32, r: f64, g: f64, b: f64, a: f64) {
    let entry = find!(ENCODERS, encoder);
    entry
        .lock()
        .unwrap()
        .colour
        .push((view, wgpu::Color { r, g, b, a }));
}

pub unsafe fn pass_depth(encoder: i32, view: i32, clear: f64, stencil_clear: i32) {
    let entry = find!(ENCODERS, encoder);
    entry.lock().unwrap().depth = Some((view, clear, stencil_clear));
}

/// Opens what was described. Anything not attached by then is not in the pass.
pub unsafe fn pass_begin(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if held.pass.is_some() || held.compute.is_some() {
        host::raise(ErrorKind::Runtime, "a pass is already open on this encoder");
        return;
    }

    // Resolved before the descriptor is built, so the views outlive it.
    let mut colour = Vec::with_capacity(held.colour.len());
    for (handle, clear) in &held.colour {
        match VIEWS.lock().unwrap().get(*handle) {
            Some(view) => colour.push((view, *clear)),
            None => return,
        }
    }
    let depth = match held.depth {
        Some((handle, clear, stencil)) => match VIEWS.lock().unwrap().get(handle) {
            Some(view) => Some((view, clear, stencil)),
            None => return,
        },
        None => None,
    };

    let attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = colour
        .iter()
        .map(|(view, clear)| {
            Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(*clear),
                    store: wgpu::StoreOp::Store,
                },
            })
        })
        .collect();

    let pass = {
        let Some(encoder) = held.encoder.as_mut() else {
            return;
        };
        encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &attachments,
                depth_stencil_attachment: depth.as_ref().map(|(view, clear, stencil)| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(*clear as f32),
                            store: wgpu::StoreOp::Store,
                        }),
                        // A depth-only format must not be given these, and
                        // one with a stencil must be.
                        stencil_ops: (*stencil >= 0).then_some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(*stencil as u32),
                            store: wgpu::StoreOp::Store,
                        }),
                    }
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: Default::default(),
            })
            .forget_lifetime()
    };
    held.pass = Some(pass);
    held.colour.clear();
    held.depth = None;
}

pub unsafe fn render_set_pipeline(encoder: i32, pipeline: i32) {
    let entry = find!(ENCODERS, encoder);
    let pipeline = find!(RENDER_PIPELINES, pipeline);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_pipeline(&pipeline);
    }
}

pub unsafe fn render_set_vertex_buffer(encoder: i32, slot: i32, buffer: i32) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_vertex_buffer(slot.max(0) as u32, buffer.slice(..));
    }
}

pub unsafe fn render_set_viewport(
    encoder: i32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    min_depth: f64,
    max_depth: f64,
) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_viewport(
            x as f32,
            y as f32,
            width as f32,
            height as f32,
            min_depth as f32,
            max_depth as f32,
        );
    }
}

pub unsafe fn render_set_scissor_rect(encoder: i32, x: i32, y: i32, width: i32, height: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_scissor_rect(
            x.max(0) as u32,
            y.max(0) as u32,
            width.max(0) as u32,
            height.max(0) as u32,
        );
    }
}

pub unsafe fn render_draw(encoder: i32, vertices: i32, instances: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.draw(0..vertices.max(0) as u32, 0..instances.max(1) as u32);
    }
}

pub unsafe fn encoder_render_end(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    // Dropping the pass is what ends it.
    entry.lock().unwrap().pass = None;
}

/// The texture side of a copy, always the whole of mip level zero.
fn whole_texture(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

fn extent(width: i32, height: i32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: width.max(1) as u32,
        height: height.max(1) as u32,
        depth_or_array_layers: 1,
    }
}

pub unsafe fn encoder_copy_buffer_to_texture(
    encoder: i32,
    buffer: i32,
    bytes_per_row: i32,
    texture: i32,
    width: i32,
    height: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let texture = find!(TEXTURES, texture);
    let mut held = entry.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row.max(0) as u32),
                rows_per_image: Some(height.max(1) as u32),
            },
        },
        whole_texture(&texture),
        extent(width, height),
    );
}

pub unsafe fn encoder_copy_texture_to_texture(
    encoder: i32,
    src: i32,
    dst: i32,
    width: i32,
    height: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let source = find!(TEXTURES, src);
    let target = find!(TEXTURES, dst);
    let mut held = entry.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    encoder.copy_texture_to_texture(
        whole_texture(&source),
        whole_texture(&target),
        extent(width, height),
    );
}

pub unsafe fn encoder_clear_buffer(encoder: i32, buffer: i32, offset: i64, size: i64) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let mut held = entry.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    encoder.clear_buffer(&buffer, offset.max(0) as u64, Some(size.max(0) as u64));
}

pub unsafe fn encoder_copy_texture_to_buffer(
    encoder: i32,
    texture: i32,
    buffer: i32,
    width: i32,
    height: i32,
    bytes_per_row: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let texture = find!(TEXTURES, texture);
    let buffer = find!(BUFFERS, buffer);
    let mut held = entry.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row.max(0) as u32),
                rows_per_image: Some(height.max(1) as u32),
            },
        },
        wgpu::Extent3d {
            width: width.max(1) as u32,
            height: height.max(1) as u32,
            depth_or_array_layers: 1,
        },
    );
}

// -- samplers ---------------------------------------------------------------

fn sampler_filter(value: i32) -> wgpu::FilterMode {
    if value == 1 {
        wgpu::FilterMode::Linear
    } else {
        wgpu::FilterMode::Nearest
    }
}

fn sampler_mipmap_filter(value: i32) -> wgpu::MipmapFilterMode {
    if value == 1 {
        wgpu::MipmapFilterMode::Linear
    } else {
        wgpu::MipmapFilterMode::Nearest
    }
}

fn sampler_address(value: i32) -> wgpu::AddressMode {
    match value {
        1 => wgpu::AddressMode::Repeat,
        2 => wgpu::AddressMode::MirrorRepeat,
        3 => wgpu::AddressMode::ClampToBorder,
        _ => wgpu::AddressMode::ClampToEdge,
    }
}

fn border_color(value: i32) -> wgpu::SamplerBorderColor {
    match value {
        1 => wgpu::SamplerBorderColor::OpaqueBlack,
        2 => wgpu::SamplerBorderColor::OpaqueWhite,
        3 => wgpu::SamplerBorderColor::Zero,
        _ => wgpu::SamplerBorderColor::TransparentBlack,
    }
}

pub unsafe fn sampler_create(device: i32, descriptor: &GpuSamplerDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    let sampler = entry.device.create_sampler(&wgpu::SamplerDescriptor {
        label: label.as_ref().map(Text::as_str),
        address_mode_u: sampler_address(descriptor.addressModeU.unwrap_or(0)),
        address_mode_v: sampler_address(descriptor.addressModeV.unwrap_or(0)),
        address_mode_w: sampler_address(descriptor.addressModeW.unwrap_or(0)),
        mag_filter: sampler_filter(descriptor.magFilter.unwrap_or(0)),
        min_filter: sampler_filter(descriptor.minFilter.unwrap_or(0)),
        mipmap_filter: sampler_mipmap_filter(descriptor.mipmapFilter.unwrap_or(0)),
        lod_min_clamp: descriptor.lodMinClamp.unwrap_or(0.0),
        lod_max_clamp: descriptor.lodMaxClamp.unwrap_or(32.0),
        compare: descriptor.compare.map(compare_function),
        anisotropy_clamp: descriptor
            .maxAnisotropy
            .unwrap_or(1)
            .clamp(1, u16::MAX as i32) as u16,
        border_color: descriptor.borderColor.map(border_color),
    });
    SAMPLERS.lock().unwrap().put(sampler)
}

pub unsafe fn sampler_destroy(sampler: i32) {
    SAMPLERS.lock().unwrap().remove(sampler);
}

pub unsafe fn queue_write_texture(
    queue: i32,
    texture: i32,
    data: Buffer,
    width: i32,
    height: i32,
    bytes_per_row: i32,
) {
    if width <= 0 || height <= 0 || bytes_per_row <= 0 {
        host::raise(
            ErrorKind::Type,
            "texture upload dimensions must be positive",
        );
        return;
    }
    let Some(len) = bytes_per_row.checked_mul(height) else {
        host::raise(ErrorKind::Type, "texture upload size overflow");
        return;
    };
    let Some(bytes) = bytes(&data, len) else {
        return;
    };
    let queue = find!(QUEUES, queue);
    let texture = find!(TEXTURES, texture);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row.max(0) as u32),
            rows_per_image: Some(height as u32),
        },
        wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
    );
}

// -- indexed drawing --------------------------------------------------------

pub unsafe fn render_set_bind_group(encoder: i32, group: i32, bindgroup: i32) {
    let entry = find!(ENCODERS, encoder);
    let bind_group = find!(BINDGROUPS, bindgroup);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_bind_group(group.max(0) as u32, &*bind_group, &[]);
    }
}

pub unsafe fn render_set_index_buffer(encoder: i32, buffer: i32, format: i32) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let format = if format == 1 {
        wgpu::IndexFormat::Uint32
    } else {
        wgpu::IndexFormat::Uint16
    };
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_index_buffer(buffer.slice(..), format);
    }
}

pub unsafe fn render_draw_indexed(encoder: i32, indices: i32, instances: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.draw_indexed(0..indices.max(0) as u32, 0, 0..instances.max(1) as u32);
    }
}

// -- surfaces ---------------------------------------------------------------

/// A surface and the frame currently acquired on it.
///
/// The frame has to be held between `acquire` and `present`: presenting is
/// what consumes it, and the view handed out points into it.
struct SurfaceEntry {
    surface: wgpu::Surface<'static>,
    frame: Option<wgpu::SurfaceTexture>,
    view: i32,
}

slab!(SURFACES, Mutex<SurfaceEntry>, Kind::Surface);

/// Puts a raw window handle back together from the integers that crossed.
///
/// The window plugin took it apart; the two libraries share no Rust type, only these
/// numbers and the platform code that says how to read them.
unsafe fn raw_handles(
    platform: i32,
    wa: i64,
    wb: i64,
    da: i64,
    db: i64,
) -> Option<(
    raw_window_handle::RawDisplayHandle,
    raw_window_handle::RawWindowHandle,
)> {
    use raw_window_handle as rwh;
    use std::ffi::c_void;
    use std::ptr::NonNull;

    Some(match platform {
        1 => (
            rwh::RawDisplayHandle::AppKit(rwh::AppKitDisplayHandle::new()),
            rwh::RawWindowHandle::AppKit(rwh::AppKitWindowHandle::new(NonNull::new(
                wa as *mut c_void,
            )?)),
        ),
        2 => {
            let mut window = rwh::Win32WindowHandle::new(std::num::NonZeroIsize::new(wa as isize)?);
            window.hinstance = std::num::NonZeroIsize::new(wb as isize);
            (
                rwh::RawDisplayHandle::Windows(rwh::WindowsDisplayHandle::new()),
                rwh::RawWindowHandle::Win32(window),
            )
        }
        3 => {
            // An Xlib id is a `c_ulong`, which is 64 bits on Unix and 32 on
            // Windows. Writing `u64` compiles on the platform this branch is
            // for and nowhere else.
            let mut window = rwh::XlibWindowHandle::new(wa as std::os::raw::c_ulong);
            window.visual_id = wb as std::os::raw::c_ulong;
            (
                rwh::RawDisplayHandle::Xlib(rwh::XlibDisplayHandle::new(
                    NonNull::new(da as *mut c_void),
                    db as i32,
                )),
                rwh::RawWindowHandle::Xlib(window),
            )
        }
        4 => (
            rwh::RawDisplayHandle::Wayland(rwh::WaylandDisplayHandle::new(NonNull::new(
                da as *mut c_void,
            )?)),
            rwh::RawWindowHandle::Wayland(rwh::WaylandWindowHandle::new(NonNull::new(
                wa as *mut c_void,
            )?)),
        ),
        5 => (
            rwh::RawDisplayHandle::Android(rwh::AndroidDisplayHandle::new()),
            rwh::RawWindowHandle::AndroidNdk(rwh::AndroidNdkWindowHandle::new(NonNull::new(
                wa as *mut c_void,
            )?)),
        ),
        6 => {
            let mut window = rwh::UiKitWindowHandle::new(NonNull::new(wa as *mut c_void)?);
            window.ui_view_controller = NonNull::new(wb as *mut c_void);
            (
                rwh::RawDisplayHandle::UiKit(rwh::UiKitDisplayHandle::new()),
                rwh::RawWindowHandle::UiKit(window),
            )
        }
        7 => (
            rwh::RawDisplayHandle::Web(rwh::WebDisplayHandle::new()),
            rwh::RawWindowHandle::WebCanvas(rwh::WebCanvasWindowHandle::new(NonNull::new(
                wa as *mut c_void,
            )?)),
        ),
        8 => (
            rwh::RawDisplayHandle::Web(rwh::WebDisplayHandle::new()),
            rwh::RawWindowHandle::WebOffscreenCanvas(rwh::WebOffscreenCanvasWindowHandle::new(
                NonNull::new(wa as *mut c_void)?,
            )),
        ),
        _ => return None,
    })
}

pub unsafe fn surface_create(
    instance: i32,
    platform: i32,
    wa: i64,
    wb: i64,
    da: i64,
    db: i64,
) -> i32 {
    let instance = find!(INSTANCES, instance, 0);
    let Some((display, window)) = (unsafe { raw_handles(platform, wa, wb, da, db) }) else {
        return 0;
    };
    // Unsafe because nothing here can prove the window outlives the surface.
    // The Haxe side owns both and closes them in order.
    let made = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(display),
            raw_window_handle: window,
        })
    };
    match made {
        Ok(surface) => SURFACES.lock().unwrap().put(Mutex::new(SurfaceEntry {
            surface,
            frame: None,
            view: 0,
        })),
        Err(_) => 0,
    }
}

pub unsafe fn surface_preferred_format(surface: i32, adapter: i32) -> i32 {
    let entry = find!(SURFACES, surface, 0);
    let adapter = find!(ADAPTERS, adapter, 0);
    let held = entry.lock().unwrap();
    let formats = held.surface.get_capabilities(&adapter).formats;
    // -1 rather than a default, so a format this library has no name for
    // cannot pass itself off as Rgba8Unorm and fail later inside configure.
    formats
        .into_iter()
        .find_map(|format| match format {
            wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Bgra8UnormSrgb => texture_format_code(format),
            _ => None,
        })
        .unwrap_or(-1)
}

pub unsafe fn surface_configure(device: i32, surface: i32, width: i32, height: i32, format: i32) {
    let entry = find!(DEVICES, device);
    let surface = find!(SURFACES, surface);
    let held = surface.lock().unwrap();
    held.surface.configure(
        &entry.device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: texture_format(format),
            width: width.max(1) as u32,
            height: height.max(1) as u32,
            color_space: wgpu::SurfaceColorSpace::Auto,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        },
    );
}

pub unsafe fn surface_acquire(surface: i32) -> i32 {
    let entry = find!(SURFACES, surface, 0);
    let mut held = entry.lock().unwrap();
    if held.frame.is_some() {
        return held.view;
    }
    // Suboptimal is still a frame -- a resize usually reports it before it
    // reports Outdated, and refusing it would drop every frame in between.
    let frame = match held.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        _ => return 0,
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let handle = VIEWS.lock().unwrap().put(view);
    held.frame = Some(frame);
    held.view = handle;
    handle
}

pub unsafe fn surface_present(queue: i32, surface: i32) {
    let queue = find!(QUEUES, queue);
    let entry = find!(SURFACES, surface);
    let mut held = entry.lock().unwrap();
    // The view points into the frame, so it goes first.
    if held.view != 0 {
        VIEWS.lock().unwrap().remove(held.view);
        held.view = 0;
    }
    if let Some(frame) = held.frame.take() {
        queue.present(frame);
    }
}

pub unsafe fn surface_destroy(surface: i32) {
    let entry = find!(SURFACES, surface);
    let mut held = entry.lock().unwrap();
    if held.view != 0 {
        VIEWS.lock().unwrap().remove(held.view);
        held.view = 0;
    }
    held.frame.take();
    drop(held);
    SURFACES.lock().unwrap().remove(surface);
}

// -- render pipeline builder ------------------------------------------------

/// A render pipeline under construction.
///
/// Built by a run of calls rather than one packed descriptor, so every value
/// crossing the boundary is a scalar the compiler checks on both sides.
#[derive(Default)]
struct PipelineBuild {
    device: i32,
    shader: i32,
    /// An explicit pipeline layout, or 0 for one inferred from the shader.
    layout: i32,
    vertex_entry: String,
    fragment_entry: String,
    buffers: Vec<(u64, wgpu::VertexStepMode, Vec<wgpu::VertexAttribute>)>,
    targets: Vec<(
        wgpu::TextureFormat,
        wgpu::ColorWrites,
        Option<wgpu::BlendState>,
    )>,
    depth: Option<(wgpu::TextureFormat, bool, wgpu::CompareFunction)>,
    stencil: Option<wgpu::StencilState>,
    primitive: wgpu::PrimitiveState,
}

slab!(BUILDERS, Mutex<PipelineBuild>, Kind::Builder);

// The orders below are the WebGPU IDL's, which is where the Haxe enums and the
// JavaScript name arrays come from too. wgpu's spelling is not derivable from
// the spec's, so this one mapping is written out.
fn blend_factor(i: i32) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as F;
    match i {
        1 => F::One,
        2 => F::Src,
        3 => F::OneMinusSrc,
        4 => F::SrcAlpha,
        5 => F::OneMinusSrcAlpha,
        6 => F::Dst,
        7 => F::OneMinusDst,
        8 => F::DstAlpha,
        9 => F::OneMinusDstAlpha,
        10 => F::SrcAlphaSaturated,
        11 => F::Constant,
        12 => F::OneMinusConstant,
        13 => F::Src1,
        14 => F::OneMinusSrc1,
        15 => F::Src1Alpha,
        16 => F::OneMinusSrc1Alpha,
        _ => F::Zero,
    }
}

fn blend_operation(i: i32) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as O;
    match i {
        1 => O::Subtract,
        2 => O::ReverseSubtract,
        3 => O::Min,
        4 => O::Max,
        _ => O::Add,
    }
}

fn compare_function(i: i32) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match i {
        0 => C::Never,
        2 => C::Equal,
        3 => C::LessEqual,
        4 => C::Greater,
        5 => C::NotEqual,
        6 => C::GreaterEqual,
        7 => C::Always,
        _ => C::Less,
    }
}

/// The order the WebGPU IDL declares them in, like every other enum here.
fn stencil_operation(i: i32) -> wgpu::StencilOperation {
    use wgpu::StencilOperation as S;
    match i {
        1 => S::Zero,
        2 => S::Replace,
        3 => S::Invert,
        4 => S::IncrementClamp,
        5 => S::DecrementClamp,
        6 => S::IncrementWrap,
        7 => S::DecrementWrap,
        _ => S::Keep,
    }
}

fn topology(i: i32) -> wgpu::PrimitiveTopology {
    use wgpu::PrimitiveTopology as T;
    match i {
        0 => T::PointList,
        1 => T::LineList,
        2 => T::LineStrip,
        4 => T::TriangleStrip,
        _ => T::TriangleList,
    }
}

/// `none` is the absence of a face rather than a third face, which is why
/// this one is an `Option` where the spec has an enum.
fn cull_mode(i: i32) -> Option<wgpu::Face> {
    match i {
        1 => Some(wgpu::Face::Front),
        2 => Some(wgpu::Face::Back),
        _ => None,
    }
}

fn front_face(i: i32) -> wgpu::FrontFace {
    match i {
        1 => wgpu::FrontFace::Cw,
        _ => wgpu::FrontFace::Ccw,
    }
}

fn step_mode(i: i32) -> wgpu::VertexStepMode {
    match i {
        1 => wgpu::VertexStepMode::Instance,
        _ => wgpu::VertexStepMode::Vertex,
    }
}

pub unsafe fn pipeline_begin(device: i32) -> i32 {
    if DEVICES.lock().unwrap().get(device).is_none() {
        return 0;
    }
    BUILDERS.lock().unwrap().put(Mutex::new(PipelineBuild {
        device,
        ..Default::default()
    }))
}

/// Runs `body` on a builder, or does nothing.
fn building(handle: i32, body: impl FnOnce(&mut PipelineBuild)) {
    let Some(entry) = BUILDERS.lock().unwrap().get(handle) else {
        return;
    };
    body(&mut entry.lock().unwrap());
}

pub unsafe fn pipeline_shader(builder: i32, shader: i32, vs: Text, fs: Text) {
    let (vs, fs) = (vs.as_str().to_owned(), fs.as_str().to_owned());
    building(builder, |build| {
        build.shader = shader;
        build.vertex_entry = vs;
        build.fragment_entry = fs;
    });
}

pub unsafe fn pipeline_layout(builder: i32, layout: i32) {
    building(builder, |build| build.layout = layout);
}

pub unsafe fn pipeline_vertex_buffer(builder: i32, stride: i64, step: i32) {
    building(builder, |build| {
        build
            .buffers
            .push((stride.max(0) as u64, step_mode(step), Vec::new()));
    });
}

pub unsafe fn pipeline_attribute(builder: i32, format: i32, offset: i64, location: i32) {
    building(builder, |build| {
        // Belongs to the buffer opened last; the Haxe builder's types are what
        // stop this being reached with none open.
        if let Some((_, _, attributes)) = build.buffers.last_mut() {
            attributes.push(wgpu::VertexAttribute {
                format: vertex_format(format),
                offset: offset.max(0) as u64,
                shader_location: location.max(0) as u32,
            });
        }
    });
}

/// Appends packed against the previous attribute, at the next free location.
///
/// Locations count across every buffer of the pipeline; offsets restart with
/// each buffer, because that is what an offset is relative to.
pub unsafe fn pipeline_attribute_packed(builder: i32, format: i32) {
    building(builder, |build| {
        let location = build
            .buffers
            .iter()
            .map(|(_, _, attributes)| attributes.len())
            .sum::<usize>() as u32;
        if let Some((_, _, attributes)) = build.buffers.last_mut() {
            let offset = attributes.last().map_or(0, |a| a.offset + a.format.size());
            attributes.push(wgpu::VertexAttribute {
                format: vertex_format(format),
                offset,
                shader_location: location,
            });
        }
    });
}

pub unsafe fn pipeline_target(builder: i32, format: i32, write_mask: i32) {
    building(builder, |build| {
        build.targets.push((
            texture_format(format),
            wgpu::ColorWrites::from_bits_truncate(write_mask as u32),
            None,
        ));
    });
}

pub unsafe fn pipeline_blend(
    builder: i32,
    src: i32,
    dst: i32,
    op: i32,
    src_alpha: i32,
    dst_alpha: i32,
    op_alpha: i32,
) {
    building(builder, |build| {
        if let Some((_, _, blend)) = build.targets.last_mut() {
            *blend = Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: blend_factor(src),
                    dst_factor: blend_factor(dst),
                    operation: blend_operation(op),
                },
                alpha: wgpu::BlendComponent {
                    src_factor: blend_factor(src_alpha),
                    dst_factor: blend_factor(dst_alpha),
                    operation: blend_operation(op_alpha),
                },
            });
        }
    });
}

pub unsafe fn pipeline_stencil(
    builder: i32,
    compare: i32,
    fail: i32,
    depth_fail: i32,
    pass_op: i32,
    read_mask: i32,
    write_mask: i32,
) {
    building(builder, |build| {
        // Both faces the same: a caller who needs them to differ is doing
        // something this has no way to say yet.
        let face = wgpu::StencilFaceState {
            compare: compare_function(compare),
            fail_op: stencil_operation(fail),
            depth_fail_op: stencil_operation(depth_fail),
            pass_op: stencil_operation(pass_op),
        };
        build.stencil = Some(wgpu::StencilState {
            front: face,
            back: face,
            read_mask: read_mask as u32,
            write_mask: write_mask as u32,
        });
    });
}

pub unsafe fn pipeline_depth(builder: i32, format: i32, write: bool, compare: i32) {
    building(builder, |build| {
        build.depth = Some((texture_format(format), write, compare_function(compare)));
    });
}

pub unsafe fn pipeline_primitive(builder: i32, topology_of: i32, cull: i32, front: i32) {
    building(builder, |build| {
        build.primitive = wgpu::PrimitiveState {
            topology: topology(topology_of),
            cull_mode: cull_mode(cull),
            front_face: front_face(front),
            ..Default::default()
        };
    });
}

pub unsafe fn render_pipeline_build(builder: i32) -> i32 {
    let Some(entry) = BUILDERS.lock().unwrap().get(builder) else {
        return 0;
    };
    let build = entry.lock().unwrap();
    let device = find!(DEVICES, build.device, 0);
    let module = find!(SHADERS, build.shader, 0);
    let layout = match build.layout {
        0 => None,
        handle => match PIPELINE_LAYOUTS.lock().unwrap().get(handle) {
            Some(layout) => Some(layout),
            None => return refuse("the pipeline layout was destroyed"),
        },
    };

    // Held so the descriptor below can borrow them.
    let layouts: Vec<wgpu::VertexBufferLayout> = build
        .buffers
        .iter()
        .map(|(stride, step, attributes)| wgpu::VertexBufferLayout {
            // Zero means the caller did not say, so it is as wide as the
            // attributes turned out to be.
            array_stride: if *stride != 0 {
                *stride
            } else {
                attributes
                    .iter()
                    .map(|a| a.offset + a.format.size())
                    .max()
                    .unwrap_or(0)
            },
            step_mode: *step,
            attributes,
        })
        .collect();
    let buffers: Vec<Option<wgpu::VertexBufferLayout>> = layouts.into_iter().map(Some).collect();
    let targets: Vec<Option<wgpu::ColorTargetState>> = build
        .targets
        .iter()
        .map(|(format, write_mask, blend)| {
            Some(wgpu::ColorTargetState {
                format: *format,
                blend: *blend,
                write_mask: *write_mask,
            })
        })
        .collect();
    let depth_stencil = build
        .depth
        .map(|(format, write, compare)| wgpu::DepthStencilState {
            format,
            depth_write_enabled: Some(write),
            depth_compare: Some(compare),
            stencil: build.stencil.clone().unwrap_or_default(),
            bias: Default::default(),
        });

    let pipeline = device
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: layout.as_deref(),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(build.vertex_entry.as_str()),
                buffers: &buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(build.fragment_entry.as_str()),
                targets: &targets,
                compilation_options: Default::default(),
            }),
            primitive: build.primitive,
            depth_stencil,
            multisample: Default::default(),
            multiview_mask: Default::default(),
            cache: None,
        });
    drop(build);
    BUILDERS.lock().unwrap().remove(builder);
    RENDER_PIPELINES.lock().unwrap().put(pipeline)
}

// -- pass state ---------------------------------------------------------------

pub unsafe fn render_set_blend_constant(encoder: i32, r: f64, g: f64, b: f64, a: f64) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_blend_constant(wgpu::Color { r, g, b, a });
    }
}

// -- drawing the GPU decided on -----------------------------------------------

pub unsafe fn render_draw_indirect(encoder: i32, buffer: i32, offset: i64) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.draw_indirect(&buffer, offset.max(0) as u64);
    }
}

pub unsafe fn render_draw_indexed_indirect(encoder: i32, buffer: i32, offset: i64) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.draw_indexed_indirect(&buffer, offset.max(0) as u64);
    }
}

// -- labels for a capture -----------------------------------------------------

/// A debug label belongs to whatever is recording: the pass while one is
/// open, the encoder otherwise. Sending an encoder one while a pass is open is
/// an error, and a caller should not have to know which they are in.
pub unsafe fn encoder_push_debug_group(encoder: i32, label: Text) {
    let entry = find!(ENCODERS, encoder);
    let label = label.as_str();
    let mut held = entry.lock().unwrap();
    match held.pass.as_mut() {
        Some(pass) => pass.push_debug_group(label),
        None => {
            if let Some(encoder) = held.encoder.as_mut() {
                encoder.push_debug_group(label);
            }
        }
    }
}

pub unsafe fn encoder_pop_debug_group(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    match held.pass.as_mut() {
        Some(pass) => pass.pop_debug_group(),
        None => {
            if let Some(encoder) = held.encoder.as_mut() {
                encoder.pop_debug_group();
            }
        }
    }
}

pub unsafe fn encoder_insert_debug_marker(encoder: i32, label: Text) {
    let entry = find!(ENCODERS, encoder);
    let label = label.as_str();
    let mut held = entry.lock().unwrap();
    match held.pass.as_mut() {
        Some(pass) => pass.insert_debug_marker(label),
        None => {
            if let Some(encoder) = held.encoder.as_mut() {
                encoder.insert_debug_marker(label);
            }
        }
    }
}

// -- stencil, indirect compute, and what the shader compiler said -------------

pub unsafe fn render_set_stencil_reference(encoder: i32, reference: i32) {
    let entry = find!(ENCODERS, encoder);
    let mut held = entry.lock().unwrap();
    if let Some(pass) = held.pass.as_mut() {
        pass.set_stencil_reference(reference.max(0) as u32);
    }
}

pub unsafe fn encoder_compute_indirect(
    encoder: i32,
    pipeline: i32,
    bindgroup: i32,
    buffer: i32,
    offset: i64,
) {
    let encoder = find!(ENCODERS, encoder);
    let pipeline = find!(PIPELINES, pipeline);
    let bind_group = find!(BINDGROUPS, bindgroup);
    let buffer = find!(BUFFERS, buffer);
    let mut held = encoder.lock().unwrap();
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };

    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: None,
        timestamp_writes: None,
    });
    pass.set_pipeline(&pipeline);
    pass.set_bind_group(0, &*bind_group, &[]);
    pass.dispatch_workgroups_indirect(&buffer, offset.max(0) as u64);
}

/// One message a line, as `line N: what`, or null if the compiler said nothing.
///
/// `device_take_error` says a shader was wrong; this says where.
pub unsafe fn shader_messages(shader: i32) -> Text {
    let module = find!(SHADERS, shader, Text::NULL);
    let info = pollster::block_on(module.get_compilation_info());
    if info.messages.is_empty() {
        return Text::NULL;
    }
    let text = info
        .messages
        .iter()
        .map(|m| match &m.location {
            Some(at) => format!("line {}: {}", at.line_number, m.message),
            None => m.message.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    text_out(&text)
}

pub unsafe fn adapter_open(instance: i32, power: i32) -> Future<crate::GpuAdapter> {
    unsafe { adapter_request(instance, power) }
}
pub unsafe fn device_open(adapter: i32) -> Future<crate::GpuDevice> {
    unsafe { device_request(adapter) }
}
pub unsafe fn device_open_with(
    adapter: i32,
    descriptor: &GpuDeviceDescriptor,
) -> Future<crate::GpuDevice> {
    unsafe { device_request_with(adapter, descriptor) }
}
pub unsafe fn adapter_driver(adapter: i32) -> Text {
    let adapter = find!(ADAPTERS, adapter, Text::NULL);
    Text::new(&adapter.get_info().driver)
}
pub unsafe fn adapter_driver_info(adapter: i32) -> Text {
    let adapter = find!(ADAPTERS, adapter, Text::NULL);
    Text::new(&adapter.get_info().driver_info)
}
pub unsafe fn pipeline_release(pipeline: i32) {
    if kind_of(pipeline) == Kind::Renderpipeline as i32 {
        unsafe {
            render_pipeline_destroy(pipeline);
        }
    } else {
        unsafe {
            pipeline_destroy(pipeline);
        }
    }
}
pub unsafe fn is_valid(handle: i32) -> bool {
    match kind_of(handle) {
        k if k == Kind::Instance as i32 => INSTANCES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Adapter as i32 => ADAPTERS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Device as i32 => DEVICES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Queue as i32 => QUEUES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Buffer as i32 => BUFFERS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Shader as i32 => SHADERS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Pipeline as i32 => PIPELINES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Renderpipeline as i32 => {
            RENDER_PIPELINES.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::Texture as i32 => TEXTURES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::View as i32 => VIEWS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Sampler as i32 => SAMPLERS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Bindgroup as i32 => BINDGROUPS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Encoder as i32 => ENCODERS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Surface as i32 => SURFACES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Builder as i32 => BUILDERS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Bindings as i32 => BINDINGS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::BindGroupLayout as i32 => {
            BIND_GROUP_LAYOUTS.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::PipelineLayout as i32 => {
            PIPELINE_LAYOUTS.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::QuerySet as i32 => QUERY_SETS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::BundleEncoder as i32 => {
            BUNDLE_ENCODERS.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::Bundle as i32 => BUNDLES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Error as i32 => ERRORS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::LostInfo as i32 => LOST_INFOS.lock().unwrap().get(handle).is_some(),
        k if k == Kind::CompilationInfo as i32 => {
            COMPILATIONS.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::SurfaceCapabilities as i32 => {
            CAPABILITIES.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::ExternalTexture as i32 => {
            EXTERNAL_TEXTURES.lock().unwrap().get(handle).is_some()
        }
        k if k == Kind::Blas as i32 => BLASES.lock().unwrap().get(handle).is_some(),
        k if k == Kind::Tlas as i32 => TLASES.lock().unwrap().get(handle).is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn webgpu_features_map_to_wgpu_capabilities() {
        assert_eq!(feature(0), None);
        assert_eq!(feature(10), Some(wgpu::Features::SHADER_F16));
        assert_eq!(feature(17), Some(wgpu::Features::SUBGROUP));
        assert_eq!(feature(18), None);
        assert_eq!(
            requested_features(&[3, 10]).unwrap(),
            wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::SHADER_F16
        );
    }

    #[test]
    fn requested_limits_preserve_defaults_and_validate_values() {
        let defaults = wgpu::Limits::default();
        let requested = requested_limits(&[(4, 1), (24, 1 << 30)]).unwrap();
        assert_eq!(requested.max_bind_groups, defaults.max_bind_groups);
        assert_eq!(requested.max_buffer_size, 1 << 30);
        assert!(requested_limits(&[(24, -1)]).is_err());
        assert!(requested_limits(&[(13, 8)]).is_err());
    }

    #[test]
    fn idl_vertex_formats_map_to_wgpu_ordinals() {
        assert_eq!(vertex_format(0), wgpu::VertexFormat::Uint8);
        assert_eq!(vertex_format(28), wgpu::VertexFormat::Float32x2);
        assert_eq!(vertex_format(39), wgpu::VertexFormat::Unorm10_10_10_2);
        assert_eq!(vertex_format(40), wgpu::VertexFormat::Unorm8x4Bgra);
    }

    #[test]
    fn idl_texture_formats_map_to_wgpu_ordinals() {
        assert_eq!(texture_format(0), wgpu::TextureFormat::R8Unorm);
        assert_eq!(texture_format(21), wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(
            texture_format(48),
            wgpu::TextureFormat::Depth32FloatStencil8
        );
        assert_eq!(texture_format(61), wgpu::TextureFormat::Bc7RgbaUnorm);
        assert_eq!(
            texture_format(100),
            wgpu::TextureFormat::Astc {
                block: wgpu::AstcBlock::B12x12,
                channel: wgpu::AstcChannel::UnormSrgb,
            }
        );
    }
}

slab!(BINDINGS, Mutex<Vec<Bound>>, Kind::Bindings);
pub unsafe fn bindings_create() -> i32 {
    BINDINGS.lock().unwrap().put(Mutex::new(Vec::new()))
}
pub unsafe fn bindings_buffer(bindings: i32, buffer: i32) {
    let bindings = find!(BINDINGS, bindings);
    let buffer = find!(BUFFERS, buffer);
    bindings.lock().unwrap().push(Bound::Buffer(buffer));
}
pub unsafe fn bindings_view(bindings: i32, view: i32) {
    let bindings = find!(BINDINGS, bindings);
    let view = find!(VIEWS, view);
    bindings.lock().unwrap().push(Bound::View(view));
}
pub unsafe fn bindings_sampler(bindings: i32, sampler: i32) {
    let bindings = find!(BINDINGS, bindings);
    let sampler = find!(SAMPLERS, sampler);
    bindings.lock().unwrap().push(Bound::Sampler(sampler));
}
pub unsafe fn bindings_destroy(bindings: i32) {
    BINDINGS.lock().unwrap().remove(bindings);
}
pub unsafe fn encoder_destroy(encoder: i32) {
    ENCODERS.lock().unwrap().remove(encoder);
}
pub unsafe fn builder_destroy(builder: i32) {
    BUILDERS.lock().unwrap().remove(builder);
}

// The groups below live in their own files; they reach this file's tables
// and helpers through `super`.
mod bundles;
mod copies;
mod diagnostics;
mod external;
mod info;
mod mesh;
mod native;
mod queries;
mod ray_tracing;
mod render;
mod surfaces;
pub use bundles::*;
pub use copies::*;
pub use diagnostics::*;
pub use external::*;
pub use info::*;
pub use mesh::*;
pub use native::*;
pub use queries::*;
pub use ray_tracing::*;
pub use render::*;
pub use surfaces::*;
