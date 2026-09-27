//! The GPU plugin in a browser: each backend function a program calls is
//! encoded onto the wire to the page's WebGPU (`crate::wire`), which the
//! GPU agent decodes and runs. Functions this module does not define raise
//! that they are not available on the web (the generated `backend`).
//!
//! Commands collect in one batch, handed to the agent through the mailbox
//! when the program waits on the GPU (a request, a read) or presents a
//! frame, so a frame reaches the page whole. An upload is copied when it is
//! made and kept until its batch is sent. Objects are kept by the agent
//! under the plugin's own handles, so making one needs no round trip; the
//! GPU itself is handle 1 and the page's canvas handle 2, which no plugin
//! handle is. The program's world watches the count of replies the agent
//! has settled, and settles their futures on the program's own thread.

use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
use std::sync::{LazyLock, Mutex};

use caribou_abi::{Buffer, BufferMut, ErrorKind, Future, Rooted, Text, Value, host};

use crate::handles::{Slab, kind_of};
use crate::types::Kind;
use crate::wire::{self, Handle, Mailbox};
use crate::{
    GpuBindGroupDescriptor, GpuBufferDescriptor, GpuComputePipelineDescriptor, GpuDeviceDescriptor,
    GpuRenderPipelineDescriptor, GpuRequestAdapterOptions, GpuShaderModuleDescriptor,
    GpuSurfaceConfiguration, GpuTextureDescriptor, GpuTextureViewDescriptor,
};

/// `navigator.gpu`, as the agent keeps it.
const GPU: Handle = Handle(1);

/// The page's canvas's WebGPU context, when the page gave the agent one.
const CANVAS: Handle = Handle(2);

/// That canvas itself, whose size is the drawing buffer's.
const CANVAS_ELEMENT: Handle = Handle(3);

/// Handles of objects the plugin makes and releases in one batch (passes,
/// command buffers, layouts it only borrows), below every plugin handle.
const TRANSIENT: u32 = 1 << 26;

/// How much a batch may hold before it goes out without a wait or a frame
/// asking for it.
const BATCH: usize = 1 << 20;

static MAILBOX: Mailbox = Mailbox::new();

/// A buffer's size and usage, which WebGPU keeps as attributes; held here
/// so reading them needs no round trip.
struct BufferEntry {
    size: i64,
    usage: i32,
}

/// An encoder, and what its next render pass attaches.
#[derive(Default)]
struct EncoderEntry {
    /// The open compute or render pass, or zero.
    compute: u32,
    render: u32,
    colour: Vec<(i32, [f64; 4])>,
    depth: Option<(i32, f64, i32)>,
}

/// The canvas's frame: its texture and the view the program draws to,
/// both released once presented.
#[derive(Default)]
struct SurfaceEntry {
    texture: u32,
    view: i32,
}

/// A render pipeline described step by step, as the native builder takes it.
#[derive(Default)]
struct Build {
    device: i32,
    shader: i32,
    vertex_entry: String,
    fragment_entry: String,
    layout: i32,
    buffers: Vec<wire::GPUVertexBufferLayout>,
    targets: Vec<wire::GPUColorTargetState>,
    depth: Option<wire::GPUDepthStencilState>,
    stencil: Option<(wire::GPUStencilFaceState, u32, u32)>,
    primitive: Option<wire::GPUPrimitiveState>,
}

struct State {
    commands: wire::Encoder,
    /// Uploads the batch's commands read, until it is sent.
    staged: Vec<Box<[u8]>>,
    /// Whether a frame is acquired and not yet presented.
    drawing: bool,
    /// Whether the host started the agent.
    agent: bool,
    next: u32,
    instances: Slab<()>,
    adapters: Slab<()>,
    /// Each device's queue, made once the device is.
    devices: Slab<AtomicI32>,
    queues: Slab<()>,
    buffers: Slab<BufferEntry>,
    shaders: Slab<()>,
    pipelines: Slab<()>,
    render_pipelines: Slab<()>,
    layouts: Slab<()>,
    bind_groups: Slab<()>,
    encoders: Slab<Mutex<EncoderEntry>>,
    bindings: Slab<Mutex<Vec<i32>>>,
    textures: Slab<()>,
    views: Slab<()>,
    surfaces: Slab<Mutex<SurfaceEntry>>,
    builders: Slab<Mutex<Build>>,
    pending: Vec<Pending>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        commands: wire::Encoder::new(),
        staged: Vec::new(),
        drawing: false,
        agent: false,
        next: 2,
        instances: Slab::new(Kind::Instance),
        adapters: Slab::new(Kind::Adapter),
        devices: Slab::new(Kind::Device),
        queues: Slab::new(Kind::Queue),
        buffers: Slab::new(Kind::Buffer),
        shaders: Slab::new(Kind::Shader),
        pipelines: Slab::new(Kind::Pipeline),
        render_pipelines: Slab::new(Kind::Renderpipeline),
        layouts: Slab::new(Kind::BindGroupLayout),
        bind_groups: Slab::new(Kind::Bindgroup),
        encoders: Slab::new(Kind::Encoder),
        bindings: Slab::new(Kind::Bindings),
        textures: Slab::new(Kind::Texture),
        views: Slab::new(Kind::View),
        surfaces: Slab::new(Kind::Surface),
        builders: Slab::new(Kind::Builder),
        pending: Vec::new(),
    })
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl State {
    /// Hand the batch to the agent and wait until it has run it.
    fn flush(&mut self) {
        MAILBOX.send(&self.commands.bytes);
        self.commands.bytes.clear();
        self.staged.clear();
    }

    /// Send the batch if it has grown large and no frame is being drawn,
    /// whose commands must reach the agent together.
    fn flush_if_full(&mut self) {
        let drawing = self.drawing;
        let held = self.commands.bytes.len() + self.staged.iter().map(|b| b.len()).sum::<usize>();
        if held > BATCH && !drawing {
            self.flush();
        }
    }

    /// `bytes` copied until the batch is sent, as the wire reads them.
    fn stage(&mut self, bytes: &[u8]) -> wire::Bytes {
        let copy: Box<[u8]> = bytes.into();
        let staged = wire::Bytes {
            address: copy.as_ptr() as usize as u32,
            len: copy.len() as u32,
        };
        self.staged.push(copy);
        staged
    }

    fn transient(&mut self) -> Handle {
        self.next = if self.next + 1 >= TRANSIENT {
            3
        } else {
            self.next + 1
        };
        Handle(self.next)
    }
}

fn handle(h: i32) -> Handle {
    Handle(h as u32)
}

/// A reply record: the agent's answer to one command, in the program's
/// memory, and the buffer that takes what it carries.
#[repr(C)]
struct Reply {
    state: AtomicI32,
    len: u32,
    address: u32,
    cap: u32,
}

impl Reply {
    fn new(address: *mut u8, cap: usize) -> Reply {
        Reply {
            state: AtomicI32::new(0),
            len: 0,
            address: address as usize as u32,
            cap: cap as u32,
        }
    }

    fn at(&self) -> u32 {
        self as *const Reply as usize as u32
    }
}

/// A promise's reply, with room for a rejection's message.
struct Promise {
    reply: Reply,
    message: [u8; 1024],
}

impl Promise {
    fn new() -> Box<Promise> {
        let mut promise = Box::new(Promise {
            reply: Reply::new(std::ptr::null_mut(), 0),
            message: [0; 1024],
        });
        promise.reply = Reply::new(promise.message.as_mut_ptr(), promise.message.len());
        promise
    }

    fn message(&self) -> String {
        match self.reply.state.load(SeqCst) {
            3 => "the GPU's error message is too long to carry".to_owned(),
            _ => {
                let len = (self.reply.len as usize).min(self.message.len());
                String::from_utf8_lossy(&self.message[..len]).into_owned()
            }
        }
    }
}

/// A future waiting on a promise, and what it resolves with.
struct Pending {
    promise: Box<Promise>,
    waiting: Waiting,
}

enum Waiting {
    Adapter(Rooted<Future<crate::GpuAdapter>>, i32),
    Device(Rooted<Future<crate::GpuDevice>>, i32),
    Done(Rooted<Future<()>>),
}

/// Settle `waiting` from `promise` once the agent answers it.
fn wait(s: &mut State, promise: Box<Promise>, waiting: Waiting) {
    s.pending.push(Pending { promise, waiting });
}

/// The world's handler for the settled count.
unsafe extern "C" fn settled(_: *mut std::ffi::c_void) {
    settle();
}

/// Settle every future whose promise the agent has settled.
fn settle() {
    let settled: Vec<Pending> = {
        let mut s = state();
        let (done, waiting) = std::mem::take(&mut s.pending)
            .into_iter()
            .partition(|p| p.promise.reply.state.load(SeqCst) != 0);
        s.pending = waiting;
        done
    };
    for p in settled {
        let resolved = p.promise.reply.state.load(SeqCst) == 1;
        match p.waiting {
            Waiting::Adapter(future, adapter) if resolved => {
                if !future
                    .get()
                    .resolve_boxed(Box::new(crate::GpuAdapter { handle: adapter }))
                {
                    forget(|s| s.adapters.remove(adapter), adapter);
                }
            }
            Waiting::Device(future, device) if resolved => {
                // The queue came with the device; it is kept under a handle
                // of its own before the program can ask for it.
                {
                    let mut s = state();
                    let queue = s.queues.put(());
                    s.commands
                        .gpu_device_get_queue(handle(device), handle(queue));
                    if let Some(entry) = s.devices.get(device) {
                        entry.store(queue, SeqCst);
                    }
                }
                if !future
                    .get()
                    .resolve_boxed(Box::new(crate::GpuDevice { handle: device }))
                {
                    unsafe { device_destroy(device) };
                }
            }
            Waiting::Done(future) if resolved => {
                future.get().resolve(Value::null());
            }
            Waiting::Adapter(future, adapter) => {
                state().adapters.remove(adapter);
                future.get().reject(Text::new(&p.promise.message()).value());
            }
            Waiting::Device(future, device) => {
                state().devices.remove(device);
                future.get().reject(Text::new(&p.promise.message()).value());
            }
            Waiting::Done(future) => {
                future.get().reject(Text::new(&p.promise.message()).value());
            }
        }
    }
}

/// Drop the plugin's handle `h` and the agent's object under it.
fn forget(remove: impl FnOnce(&mut State), h: i32) {
    let mut s = state();
    remove(&mut s);
    s.commands.release(handle(h));
}

fn rejected<T>(message: &str) -> Future<T> {
    let future = Future::new();
    future.reject(Text::new(message).value());
    future
}

/// The first `len` bytes of `data`, checked.
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

/// A descriptor as the wire's, or raised as what the web lacks.
fn converted<T>(descriptor: Result<T, String>) -> Option<T> {
    descriptor
        .map_err(|message| host::raise(ErrorKind::Runtime, &message))
        .ok()
}

fn unavailable<T>(what: &str) -> Result<T, String> {
    Err(format!("`{what}` is not available on the web"))
}

/// The plugin's power preference, whose codes are its own.
fn power_preference(power: i32) -> Option<wire::GPUPowerPreference> {
    match power {
        1 => Some(wire::GPUPowerPreference::HighPerformance),
        0 => Some(wire::GPUPowerPreference::LowPower),
        _ => None,
    }
}

fn adapter_options(o: &GpuRequestAdapterOptions) -> Result<wire::GPURequestAdapterOptions, String> {
    if o.compatibleSurface.is_some() {
        return unavailable("GpuRequestAdapterOptions.compatibleSurface");
    }
    Ok(wire::GPURequestAdapterOptions {
        feature_level: None,
        power_preference: o.powerPreference.and_then(power_preference),
        force_fallback_adapter: o.forceFallbackAdapter,
        xr_compatible: None,
    })
}

/// WebGPU's features and limits; wgpu's own, and its acceptances of
/// behaviour a browser does not have, are not.
fn device_descriptor(d: &GpuDeviceDescriptor) -> Result<wire::GPUDeviceDescriptor, String> {
    if !d.requiredNativeFeatures.is_empty() {
        return unavailable("GpuDeviceDescriptor.requiredNativeFeatures");
    }
    if !d.requiredNativeLimits.is_empty() {
        return unavailable("GpuDeviceDescriptor.requiredNativeLimits");
    }
    // Imported enums: a value's code is its index in the IDL.
    let features = d
        .requiredFeatures
        .iter()
        .map(|&f| {
            wire::GPUFeatureName::from_index(f as u32)
                .ok_or("a feature this browser's WebGPU lacks")
        })
        .collect::<Result<Vec<_>, _>>()?;
    // A limit is named as GPUSupportedLimits names its attribute.
    let limits = d
        .requiredLimits
        .iter()
        .map(|&(limit, value)| {
            let variant = crate::Limit::from_native(limit).ok_or("an unknown limit")?;
            let name = format!("{variant:?}");
            let mut chars = name.chars();
            let first = chars
                .next()
                .map(|c| c.to_ascii_lowercase())
                .unwrap_or_default();
            Ok((
                format!("{first}{}", chars.as_str()),
                wire::U64OrUndefined::U64(value.max(0) as u64),
            ))
        })
        .collect::<Result<Vec<_>, &str>>()?;
    Ok(wire::GPUDeviceDescriptor {
        label: None,
        required_features: Some(features),
        required_limits: Some(limits),
        default_queue: None,
    })
}

/// A browser checks every shader, so a check can be left on but not off.
fn shader_descriptor(
    d: &GpuShaderModuleDescriptor,
) -> Result<wire::GPUShaderModuleDescriptor, String> {
    for (check, name) in [
        (d.boundsChecks, "boundsChecks"),
        (d.forceLoopBounding, "forceLoopBounding"),
        (
            d.rayQueryInitializationTracking,
            "rayQueryInitializationTracking",
        ),
        (d.taskShaderDispatchTracking, "taskShaderDispatchTracking"),
        (
            d.meshShaderPrimitiveIndicesClamp,
            "meshShaderPrimitiveIndicesClamp",
        ),
        (d.intDivChecks, "intDivChecks"),
    ] {
        if check == Some(false) {
            return unavailable(&format!("GpuShaderModuleDescriptor.{name}(false)"));
        }
    }
    Ok(wire::GPUShaderModuleDescriptor {
        label: d.label.as_ref().map(|l| l.get().as_str().to_owned()),
        code: d.code.get().as_str().to_owned(),
        compilation_hints: None,
    })
}

// -- instance and adapter ---------------------------------------------------

pub unsafe fn instance_create() -> i32 {
    let mut s = state();
    if !s.agent {
        // Watched before any request can be answered, so none is missed.
        // The mailbox is static, and `settled` needs no context.
        let wake = unsafe {
            host::watch(
                MAILBOX.settled.as_ptr().cast_const().cast(),
                settled,
                std::ptr::null_mut(),
            )
        };
        if wake.is_null() {
            host::raise(
                ErrorKind::Runtime,
                "gpu: no world to settle the GPU's answers on",
            );
            return 0;
        }
        MAILBOX.wake.store(wake as usize as u32, SeqCst);
        s.agent = host::agent("gpu", &MAILBOX as *const Mailbox as usize);
        if !s.agent {
            host::raise(
                ErrorKind::Runtime,
                "gpu: this program's host starts no GPU agent",
            );
            return 0;
        }
    }
    s.instances.put(())
}

pub unsafe fn instance_destroy(instance: i32) {
    state().instances.remove(instance);
}

fn request_adapter(
    instance: i32,
    options: Option<wire::GPURequestAdapterOptions>,
) -> Future<crate::GpuAdapter> {
    let mut s = state();
    if s.instances.get(instance).is_none() {
        return rejected("instance was destroyed");
    }
    let adapter = s.adapters.put(());
    let promise = Promise::new();
    s.commands
        .gpu_request_adapter(GPU, handle(adapter), promise.reply.at(), &options);
    s.flush();
    let future = Future::new();
    wait(
        &mut s,
        promise,
        Waiting::Adapter(Rooted::new(future), adapter),
    );
    future
}

pub unsafe fn adapter_open(instance: i32, power: i32) -> Future<crate::GpuAdapter> {
    request_adapter(
        instance,
        Some(wire::GPURequestAdapterOptions {
            feature_level: None,
            power_preference: power_preference(power),
            force_fallback_adapter: None,
            xr_compatible: None,
        }),
    )
}

pub unsafe fn adapter_request_with(
    instance: i32,
    options: &GpuRequestAdapterOptions,
) -> Future<crate::GpuAdapter> {
    match adapter_options(options) {
        Ok(options) => request_adapter(instance, Some(options)),
        Err(message) => rejected(&message),
    }
}

pub unsafe fn adapter_destroy(adapter: i32) {
    forget(|s| s.adapters.remove(adapter), adapter);
}

// -- device -----------------------------------------------------------------

fn request_device(
    adapter: i32,
    descriptor: Option<wire::GPUDeviceDescriptor>,
) -> Future<crate::GpuDevice> {
    let mut s = state();
    if s.adapters.get(adapter).is_none() {
        return rejected("adapter was destroyed");
    }
    let device = s.devices.put(AtomicI32::new(0));
    let promise = Promise::new();
    s.commands.gpu_adapter_request_device(
        handle(adapter),
        handle(device),
        promise.reply.at(),
        &descriptor,
    );
    s.flush();
    let future = Future::new();
    wait(
        &mut s,
        promise,
        Waiting::Device(Rooted::new(future), device),
    );
    future
}

pub unsafe fn device_open(adapter: i32) -> Future<crate::GpuDevice> {
    request_device(adapter, None)
}

pub unsafe fn device_open_with(
    adapter: i32,
    descriptor: &GpuDeviceDescriptor,
) -> Future<crate::GpuDevice> {
    match device_descriptor(descriptor) {
        Ok(descriptor) => request_device(adapter, Some(descriptor)),
        Err(message) => rejected(&message),
    }
}

pub unsafe fn device_queue(device: i32) -> i32 {
    state()
        .devices
        .get(device)
        .map_or(0, |entry| entry.load(SeqCst))
}

/// Commands go out as the program needs them; this sends what waits.
pub unsafe fn device_poll(_device: i32) {
    state().flush();
}

pub unsafe fn device_destroy(device: i32) {
    let mut s = state();
    let Some(entry) = s.devices.get(device) else {
        return;
    };
    let queue = entry.load(SeqCst);
    s.devices.remove(device);
    s.queues.remove(queue);
    s.commands.gpu_device_destroy(handle(device));
    s.commands.release(handle(queue));
    s.commands.release(handle(device));
}

pub unsafe fn queue_work_done(device: i32, queue: i32) -> Future<()> {
    let mut s = state();
    if s.devices.get(device).is_none() {
        return rejected("device was destroyed");
    }
    if s.queues.get(queue).is_none() {
        return rejected("queue was destroyed");
    }
    let promise = Promise::new();
    s.commands
        .gpu_queue_on_submitted_work_done(handle(queue), promise.reply.at());
    s.flush();
    let future = Future::new();
    wait(&mut s, promise, Waiting::Done(Rooted::new(future)));
    future
}

// -- buffers ----------------------------------------------------------------

pub unsafe fn buffer_create(device: i32, descriptor: &GpuBufferDescriptor) -> i32 {
    let Some(wired) = converted(descriptor.wire()) else {
        return 0;
    };
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let buffer = s.buffers.put(BufferEntry {
        size: descriptor.size,
        usage: descriptor.usage,
    });
    s.commands
        .gpu_device_create_buffer(handle(device), handle(buffer), &wired);
    buffer
}

pub unsafe fn buffer_size(buffer: i32) -> i64 {
    state().buffers.get(buffer).map_or(0, |entry| entry.size)
}

pub unsafe fn buffer_usage(buffer: i32) -> i32 {
    state().buffers.get(buffer).map_or(0, |entry| entry.usage)
}

pub unsafe fn queue_write_buffer(queue: i32, buffer: i32, offset: i64, data: Buffer, len: i32) {
    let Some(data) = bytes(&data, len) else {
        return;
    };
    if data.is_empty() {
        return;
    }
    let mut s = state();
    if s.queues.get(queue).is_none() || s.buffers.get(buffer).is_none() {
        return;
    }
    let staged = s.stage(data);
    s.commands.gpu_queue_write_buffer(
        handle(queue),
        &handle(buffer),
        &(offset.max(0) as u64),
        &staged,
        &None,
        &None,
    );
    s.flush_if_full();
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
    // GPUMapMode's READ and WRITE are the plugin's 1 and 2.
    if mode != 1 && mode != 2 {
        return rejected("a map mode is READ or WRITE");
    }
    let mut s = state();
    if s.buffers.get(buffer).is_none() {
        return rejected("buffer was destroyed");
    }
    if s.devices.get(device).is_none() {
        return rejected("device was destroyed");
    }
    let promise = Promise::new();
    s.commands.gpu_buffer_map_async(
        handle(buffer),
        promise.reply.at(),
        &(mode as u32),
        &Some(offset.max(0) as u64),
        &Some(size.max(0) as u64),
    );
    s.flush();
    let future = Future::new();
    wait(&mut s, promise, Waiting::Done(Rooted::new(future)));
    future
}

/// The mapped range, straight into `out`.
pub unsafe fn buffer_copy_out(buffer: i32, offset: i64, out: BufferMut, len: i32) -> bool {
    if len <= 0 || bytes(&out.buffer(), len).is_none() {
        return false;
    }
    let mut s = state();
    if s.buffers.get(buffer).is_none() {
        return false;
    }
    let reply = Reply::new(out.as_mut_ptr(), len as usize);
    s.commands.gpu_buffer_get_mapped_range(
        handle(buffer),
        reply.at(),
        &Some(offset.max(0) as u64),
        &Some(len as u64),
    );
    s.flush();
    reply.state.load(SeqCst) == 1 && reply.len == len as u32
}

pub unsafe fn buffer_unmap(buffer: i32) {
    let mut s = state();
    if s.buffers.get(buffer).is_some() {
        s.commands.gpu_buffer_unmap(handle(buffer));
    }
}

pub unsafe fn buffer_destroy(buffer: i32) {
    let mut s = state();
    if s.buffers.get(buffer).is_none() {
        return;
    }
    s.buffers.remove(buffer);
    s.commands.gpu_buffer_destroy(handle(buffer));
    s.commands.release(handle(buffer));
}

// -- shaders and pipelines --------------------------------------------------

fn create_shader(device: i32, descriptor: &wire::GPUShaderModuleDescriptor) -> i32 {
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let shader = s.shaders.put(());
    s.commands
        .gpu_device_create_shader_module(handle(device), handle(shader), descriptor);
    shader
}

pub unsafe fn shader_create(device: i32, wgsl: Text) -> i32 {
    create_shader(
        device,
        &wire::GPUShaderModuleDescriptor {
            label: None,
            code: wgsl.as_str().to_owned(),
            compilation_hints: None,
        },
    )
}

pub unsafe fn shader_create_with(device: i32, descriptor: &GpuShaderModuleDescriptor) -> i32 {
    converted(shader_descriptor(descriptor)).map_or(0, |d| create_shader(device, &d))
}

pub unsafe fn shader_destroy(shader: i32) {
    forget(|s| s.shaders.remove(shader), shader);
}

/// A compute pipeline descriptor's layout: unset is "auto", as the plugin
/// declares it.
pub fn gpu_compute_pipeline_descriptor_layout(
    layout: &Option<i32>,
) -> Result<wire::GPUPipelineLayoutOrGPUAutoLayoutMode, String> {
    Ok(match layout {
        Some(layout) => {
            wire::GPUPipelineLayoutOrGPUAutoLayoutMode::GPUPipelineLayout(handle(*layout))
        }
        None => wire::GPUPipelineLayoutOrGPUAutoLayoutMode::GPUAutoLayoutMode(
            wire::GPUAutoLayoutMode::Auto,
        ),
    })
}

fn create_compute_pipeline(device: i32, descriptor: &wire::GPUComputePipelineDescriptor) -> i32 {
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let pipeline = s.pipelines.put(());
    s.commands
        .gpu_device_create_compute_pipeline(handle(device), handle(pipeline), descriptor);
    pipeline
}

pub unsafe fn compute_pipeline_create(device: i32, shader: i32, entry: Text) -> i32 {
    create_compute_pipeline(
        device,
        &wire::GPUComputePipelineDescriptor {
            label: None,
            layout: wire::GPUPipelineLayoutOrGPUAutoLayoutMode::GPUAutoLayoutMode(
                wire::GPUAutoLayoutMode::Auto,
            ),
            compute: wire::GPUProgrammableStage {
                module: handle(shader),
                entry_point: Some(entry.as_str().to_owned()),
                constants: None,
            },
        },
    )
}

pub unsafe fn compute_pipeline_create_with(
    device: i32,
    descriptor: &GpuComputePipelineDescriptor,
) -> i32 {
    converted(descriptor.wire()).map_or(0, |d| create_compute_pipeline(device, &d))
}

/// A compute or a render pipeline; the handle says which.
pub unsafe fn pipeline_release(pipeline: i32) {
    forget(
        |s| {
            s.pipelines.remove(pipeline);
            s.render_pipelines.remove(pipeline);
        },
        pipeline,
    );
}

/// The layout of `pipeline`'s group `index` under `layout`, for a compute
/// or a render pipeline; false when it is neither.
fn get_bind_group_layout(s: &mut State, pipeline: i32, index: i32, layout: Handle) -> bool {
    let index = index.max(0) as u32;
    if s.render_pipelines.get(pipeline).is_some() {
        s.commands
            .gpu_render_pipeline_get_bind_group_layout(handle(pipeline), layout, &index);
    } else if s.pipelines.get(pipeline).is_some() {
        s.commands
            .gpu_compute_pipeline_get_bind_group_layout(handle(pipeline), layout, &index);
    } else {
        return false;
    }
    true
}

pub unsafe fn pipeline_bind_group_layout(pipeline: i32, index: i32) -> i32 {
    let mut s = state();
    let layout = s.layouts.put(());
    if !get_bind_group_layout(&mut s, pipeline, index, handle(layout)) {
        s.layouts.remove(layout);
        return 0;
    }
    layout
}

pub unsafe fn bind_group_layout_destroy(layout: i32) {
    forget(|s| s.layouts.remove(layout), layout);
}

// -- bind groups ------------------------------------------------------------

pub unsafe fn bindings_create() -> i32 {
    state().bindings.put(Mutex::new(Vec::new()))
}

pub unsafe fn bindings_buffer(bindings: i32, buffer: i32) {
    let s = state();
    if let Some(list) = s.bindings.get(bindings)
        && s.buffers.get(buffer).is_some()
    {
        list.lock().unwrap().push(buffer);
    }
}

pub unsafe fn bindings_destroy(bindings: i32) {
    state().bindings.remove(bindings);
}

/// A bind group over `bindings`, in order, laid out as `pipeline`'s group.
pub unsafe fn bind_group_create(device: i32, pipeline: i32, group: i32, bindings: i32) -> i32 {
    let mut s = state();
    let Some(list) = s.bindings.get(bindings) else {
        return 0;
    };
    if s.devices.get(device).is_none() {
        return 0;
    }
    let entries: Vec<_> = list
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(binding, &buffer)| wire::GPUBindGroupEntry {
            binding: binding as u32,
            resource: wire::GPUBindingResource::GPUBuffer(handle(buffer)),
        })
        .collect();
    let layout = s.transient();
    if !get_bind_group_layout(&mut s, pipeline, group, layout) {
        return 0;
    }
    let bind_group = s.bind_groups.put(());
    s.commands.gpu_device_create_bind_group(
        handle(device),
        handle(bind_group),
        &wire::GPUBindGroupDescriptor {
            label: None,
            layout,
            entries,
        },
    );
    s.commands.release(layout);
    bind_group
}

pub unsafe fn bind_group_create_with(device: i32, descriptor: &GpuBindGroupDescriptor) -> i32 {
    let Some(wired) = converted(descriptor.wire()) else {
        return 0;
    };
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let bind_group = s.bind_groups.put(());
    s.commands
        .gpu_device_create_bind_group(handle(device), handle(bind_group), &wired);
    bind_group
}

pub unsafe fn bind_group_destroy(bind_group: i32) {
    forget(|s| s.bind_groups.remove(bind_group), bind_group);
}

// -- encoders ---------------------------------------------------------------

pub unsafe fn encoder_create(device: i32) -> i32 {
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let encoder = s.encoders.put(Mutex::new(EncoderEntry::default()));
    s.commands
        .gpu_device_create_command_encoder(handle(device), handle(encoder), &None);
    encoder
}

pub unsafe fn encoder_destroy(encoder: i32) {
    forget(|s| s.encoders.remove(encoder), encoder);
}

/// Run `body` on `encoder`'s entry and the batch, or do nothing.
fn encoding(encoder: i32, body: impl FnOnce(&mut EncoderEntry, &mut State)) {
    let mut s = state();
    let Some(entry) = s.encoders.get(encoder) else {
        return;
    };
    let mut entry = entry.lock().unwrap();
    body(&mut entry, &mut s);
}

/// The open compute pass's commands, when one is open.
fn in_compute(encoder: i32, body: impl FnOnce(Handle, &mut wire::Encoder)) {
    encoding(encoder, |entry, s| {
        if entry.compute != 0 {
            body(Handle(entry.compute), &mut s.commands);
        }
    });
}

/// The open render pass's commands, when one is open.
fn in_render(encoder: i32, body: impl FnOnce(Handle, &mut wire::Encoder)) {
    encoding(encoder, |entry, s| {
        if entry.render != 0 {
            body(Handle(entry.render), &mut s.commands);
        }
    });
}

fn pass_open(entry: &EncoderEntry) -> bool {
    if entry.compute != 0 || entry.render != 0 {
        host::raise(
            ErrorKind::Runtime,
            "gpu: a pass is already open on this encoder",
        );
        return true;
    }
    false
}

pub unsafe fn compute_begin(encoder: i32) {
    encoding(encoder, |entry, s| {
        if pass_open(entry) {
            return;
        }
        let pass = s.transient();
        s.commands
            .gpu_command_encoder_begin_compute_pass(handle(encoder), pass, &None);
        entry.compute = pass.0;
    });
}

pub unsafe fn compute_set_pipeline(encoder: i32, pipeline: i32) {
    in_compute(encoder, |pass, c| {
        c.gpu_compute_pass_encoder_set_pipeline(pass, &handle(pipeline))
    });
}

pub unsafe fn compute_set_bind_group(encoder: i32, group: i32, bind_group: i32) {
    in_compute(encoder, |pass, c| {
        c.gpu_compute_pass_encoder_set_bind_group(
            pass,
            &(group.max(0) as u32),
            &Some(handle(bind_group)),
            &None,
        )
    });
}

pub unsafe fn compute_dispatch(encoder: i32, x: i32, y: i32, z: i32) {
    in_compute(encoder, |pass, c| {
        c.gpu_compute_pass_encoder_dispatch_workgroups(
            pass,
            &(x.max(0) as u32),
            &Some(y.max(0) as u32),
            &Some(z.max(0) as u32),
        )
    });
}

/// End whichever pass is open on the encoder.
fn end_pass(entry: &mut EncoderEntry, s: &mut State) {
    if entry.compute != 0 {
        s.commands
            .gpu_compute_pass_encoder_end(Handle(entry.compute));
        s.commands.release(Handle(entry.compute));
        entry.compute = 0;
    }
    if entry.render != 0 {
        s.commands.gpu_render_pass_encoder_end(Handle(entry.render));
        s.commands.release(Handle(entry.render));
        entry.render = 0;
    }
}

pub unsafe fn compute_end(encoder: i32) {
    encoding(encoder, end_pass);
}

pub unsafe fn encoder_compute(
    encoder: i32,
    pipeline: i32,
    bind_group: i32,
    x: i32,
    y: i32,
    z: i32,
) {
    unsafe {
        compute_begin(encoder);
        compute_set_pipeline(encoder, pipeline);
        compute_set_bind_group(encoder, 0, bind_group);
        compute_dispatch(encoder, x, y, z);
        compute_end(encoder);
    }
}

pub unsafe fn encoder_copy_buffer(
    encoder: i32,
    src: i32,
    src_offset: i64,
    dst: i32,
    dst_offset: i64,
    size: i64,
) {
    encoding(encoder, |_, s| {
        if s.buffers.get(src).is_none() || s.buffers.get(dst).is_none() {
            return;
        }
        s.commands.gpu_command_encoder_copy_buffer_to_buffer_2(
            handle(encoder),
            &handle(src),
            &(src_offset.max(0) as u64),
            &handle(dst),
            &(dst_offset.max(0) as u64),
            &Some(size.max(0) as u64),
        );
    });
}

/// Finish the encoder and submit it; it is spent either way. The commands
/// go out with the batch.
pub unsafe fn encoder_submit(encoder: i32, queue: i32) {
    let mut s = state();
    let Some(entry) = s.encoders.get(encoder) else {
        return;
    };
    end_pass(&mut entry.lock().unwrap(), &mut s);
    s.encoders.remove(encoder);
    if s.queues.get(queue).is_some() {
        let commands = s.transient();
        s.commands
            .gpu_command_encoder_finish(handle(encoder), commands, &None);
        s.commands.gpu_queue_submit(handle(queue), &vec![commands]);
        s.commands.release(commands);
    }
    s.commands.release(handle(encoder));
    s.flush_if_full();
}

// -- render passes ----------------------------------------------------------

pub unsafe fn pass_colour(encoder: i32, view: i32, r: f64, g: f64, b: f64, a: f64) {
    encoding(encoder, |entry, _| entry.colour.push((view, [r, g, b, a])));
}

pub unsafe fn pass_depth(encoder: i32, view: i32, clear: f64, stencil_clear: i32) {
    encoding(encoder, |entry, _| {
        entry.depth = Some((view, clear, stencil_clear))
    });
}

/// Opens what was described: each colour attachment cleared and stored,
/// and the depth attachment, with its stencil when it was given a clear.
pub unsafe fn pass_begin(encoder: i32) {
    encoding(encoder, |entry, s| {
        if pass_open(entry) {
            return;
        }
        let color_attachments = entry
            .colour
            .drain(..)
            .map(|(view, [r, g, b, a])| {
                Some(wire::GPURenderPassColorAttachment {
                    view: wire::GPUTextureOrGPUTextureView::GPUTextureView(handle(view)),
                    depth_slice: None,
                    resolve_target: None,
                    clear_value: Some(wire::GPUColor::GPUColorDict(wire::GPUColorDict {
                        r,
                        g,
                        b,
                        a,
                    })),
                    load_op: wire::GPULoadOp::Clear,
                    store_op: wire::GPUStoreOp::Store,
                })
            })
            .collect();
        let depth_stencil_attachment = entry.depth.take().map(|(view, clear, stencil)| {
            let stencil = stencil >= 0;
            wire::GPURenderPassDepthStencilAttachment {
                view: wire::GPUTextureOrGPUTextureView::GPUTextureView(handle(view)),
                depth_clear_value: Some(clear as f32),
                depth_load_op: Some(wire::GPULoadOp::Clear),
                depth_store_op: Some(wire::GPUStoreOp::Store),
                depth_read_only: None,
                stencil_clear_value: stencil.then_some(0),
                stencil_load_op: stencil.then_some(wire::GPULoadOp::Clear),
                stencil_store_op: stencil.then_some(wire::GPUStoreOp::Store),
                stencil_read_only: None,
            }
        });
        let pass = s.transient();
        s.commands.gpu_command_encoder_begin_render_pass(
            handle(encoder),
            pass,
            &wire::GPURenderPassDescriptor {
                label: None,
                color_attachments,
                depth_stencil_attachment,
                occlusion_query_set: None,
                timestamp_writes: None,
                max_draw_count: None,
            },
        );
        entry.render = pass.0;
    });
}

pub unsafe fn render_set_pipeline(encoder: i32, pipeline: i32) {
    in_render(encoder, |pass, c| {
        c.gpu_render_pass_encoder_set_pipeline(pass, &handle(pipeline))
    });
}

pub unsafe fn render_set_vertex_buffer(encoder: i32, slot: i32, buffer: i32) {
    in_render(encoder, |pass, c| {
        c.gpu_render_pass_encoder_set_vertex_buffer(
            pass,
            &(slot.max(0) as u32),
            &Some(handle(buffer)),
            &None,
            &None,
        )
    });
}

pub unsafe fn render_set_index_buffer(encoder: i32, buffer: i32, format: i32) {
    let format = if format == 1 {
        wire::GPUIndexFormat::Uint32
    } else {
        wire::GPUIndexFormat::Uint16
    };
    in_render(encoder, |pass, c| {
        c.gpu_render_pass_encoder_set_index_buffer(pass, &handle(buffer), &format, &None, &None)
    });
}

pub unsafe fn render_set_bind_group(encoder: i32, group: i32, bind_group: i32) {
    in_render(encoder, |pass, c| {
        c.gpu_render_pass_encoder_set_bind_group(
            pass,
            &(group.max(0) as u32),
            &Some(handle(bind_group)),
            &None,
        )
    });
}

pub unsafe fn render_draw(encoder: i32, vertices: i32, instances: i32) {
    in_render(encoder, |pass, c| {
        c.gpu_render_pass_encoder_draw(
            pass,
            &(vertices.max(0) as u32),
            &Some(instances.max(1) as u32),
            &None,
            &None,
        )
    });
}

pub unsafe fn render_draw_indexed(encoder: i32, indices: i32, instances: i32) {
    in_render(encoder, |pass, c| {
        c.gpu_render_pass_encoder_draw_indexed(
            pass,
            &(indices.max(0) as u32),
            &Some(instances.max(1) as u32),
            &None,
            &None,
            &None,
        )
    });
}

pub unsafe fn encoder_render_end(encoder: i32) {
    encoding(encoder, end_pass);
}

// -- textures ---------------------------------------------------------------

pub unsafe fn texture_create(device: i32, descriptor: &GpuTextureDescriptor) -> i32 {
    let Some(wired) = converted(descriptor.wire()) else {
        return 0;
    };
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let texture = s.textures.put(());
    s.commands
        .gpu_device_create_texture(handle(device), handle(texture), &wired);
    texture
}

pub unsafe fn texture_view(texture: i32, descriptor: &GpuTextureViewDescriptor) -> i32 {
    let Some(wired) = converted(descriptor.wire()) else {
        return 0;
    };
    let mut s = state();
    if s.textures.get(texture).is_none() {
        return 0;
    }
    let view = s.views.put(());
    s.commands
        .gpu_texture_create_view(handle(texture), handle(view), &Some(wired));
    view
}

pub unsafe fn texture_destroy(texture: i32) {
    let mut s = state();
    if s.textures.get(texture).is_none() {
        return;
    }
    s.textures.remove(texture);
    s.commands.gpu_texture_destroy(handle(texture));
    s.commands.release(handle(texture));
}

pub unsafe fn view_destroy(view: i32) {
    forget(|s| s.views.remove(view), view);
}

// -- surfaces ---------------------------------------------------------------

/// The page's canvas, whatever window handles the program passes: in a
/// browser that is the only surface there is.
pub unsafe fn surface_create(
    instance: i32,
    _platform: i32,
    _wa: i64,
    _wb: i64,
    _da: i64,
    _db: i64,
) -> i32 {
    let mut s = state();
    if s.instances.get(instance).is_none() {
        return 0;
    }
    s.surfaces.put(Mutex::new(SurfaceEntry::default()))
}

pub unsafe fn surface_destroy(surface: i32) {
    let mut s = state();
    if s.surfaces.get(surface).is_some() {
        s.surfaces.remove(surface);
        s.commands.gpu_canvas_context_unconfigure(CANVAS);
    }
}

/// The browser's preferred canvas format, as the plugin's code, which is
/// its index in the IDL.
pub unsafe fn surface_preferred_format(surface: i32, _adapter: i32) -> i32 {
    let mut s = state();
    if s.surfaces.get(surface).is_none() {
        return -1;
    }
    let mut format = [0u8; 4];
    let reply = Reply::new(format.as_mut_ptr(), format.len());
    s.commands.gpu_get_preferred_canvas_format(GPU, reply.at());
    s.flush();
    if reply.state.load(SeqCst) != 1 {
        return -1;
    }
    u32::from_le_bytes(format) as i32
}

fn configure(
    device: i32,
    surface: i32,
    size: (i32, i32),
    format: i32,
    usage: Option<i32>,
    view_formats: &[i32],
    alpha: Option<i32>,
) {
    let mut s = state();
    if s.devices.get(device).is_none() || s.surfaces.get(surface).is_none() {
        return;
    }
    let Some(format) = wire::GPUTextureFormat::from_index(format as u32) else {
        host::raise(
            ErrorKind::Runtime,
            "gpu: the canvas cannot take this texture format",
        );
        return;
    };
    let view_formats = view_formats
        .iter()
        .filter_map(|&f| wire::GPUTextureFormat::from_index(f as u32))
        .collect();
    // The plugin's AlphaMode: Opaque 1, PreMultiplied 2; the rest are the
    // browser's default.
    let alpha_mode = match alpha {
        Some(1) => Some(wire::GPUCanvasAlphaMode::Opaque),
        Some(2) => Some(wire::GPUCanvasAlphaMode::Premultiplied),
        _ => None,
    };
    // The drawing buffer takes the configuration's size, in physical pixels,
    // as a native surface does; the page keeps the canvas's CSS size.
    let (width, height) = size;
    if width > 0 && height > 0 {
        s.commands
            .offscreen_canvas_set_width(CANVAS_ELEMENT, &(width as u64));
        s.commands
            .offscreen_canvas_set_height(CANVAS_ELEMENT, &(height as u64));
    }
    s.commands.gpu_canvas_context_configure(
        CANVAS,
        &wire::GPUCanvasConfiguration {
            device: handle(device),
            format,
            usage: usage.map(|u| u as u32),
            view_formats: Some(view_formats),
            tone_mapping: None,
            alpha_mode,
        },
    );
}

pub unsafe fn surface_configure(device: i32, surface: i32, width: i32, height: i32, format: i32) {
    configure(device, surface, (width, height), format, None, &[], None);
}

pub unsafe fn surface_configure_with(
    device: i32,
    surface: i32,
    configuration: &GpuSurfaceConfiguration,
) {
    configure(
        device,
        surface,
        (configuration.width, configuration.height),
        configuration.format,
        configuration.usage,
        &configuration.viewFormats,
        configuration.alphaMode,
    );
}

/// The canvas's texture for this frame, and a view of it to draw to.
pub unsafe fn surface_acquire(surface: i32) -> i32 {
    let mut s = state();
    let Some(entry) = s.surfaces.get(surface) else {
        return 0;
    };
    let mut frame = entry.lock().unwrap();
    if frame.view != 0 {
        return frame.view;
    }
    let texture = s.transient();
    let view = s.views.put(());
    s.commands
        .gpu_canvas_context_get_current_texture(CANVAS, texture);
    s.commands
        .gpu_texture_create_view(texture, handle(view), &None);
    frame.texture = texture.0;
    frame.view = view;
    s.drawing = true;
    view
}

/// Send the frame: the agent runs it, and the page shows it.
pub unsafe fn surface_present(_queue: i32, surface: i32) {
    let mut s = state();
    let Some(entry) = s.surfaces.get(surface) else {
        return;
    };
    let mut frame = entry.lock().unwrap();
    if frame.view != 0 {
        let view = frame.view;
        s.views.remove(view);
        s.commands.release(handle(view));
        s.commands.release(Handle(frame.texture));
        frame.view = 0;
        frame.texture = 0;
    }
    s.drawing = false;
    s.flush();
}

// -- render pipelines -------------------------------------------------------

/// A render pipeline descriptor's layout: unset is "auto", as the plugin
/// declares it.
pub fn gpu_render_pipeline_descriptor_layout(
    layout: &Option<i32>,
) -> Result<wire::GPUPipelineLayoutOrGPUAutoLayoutMode, String> {
    gpu_compute_pipeline_descriptor_layout(layout)
}

fn create_render_pipeline(device: i32, descriptor: &wire::GPURenderPipelineDescriptor) -> i32 {
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    let pipeline = s.render_pipelines.put(());
    s.commands
        .gpu_device_create_render_pipeline(handle(device), handle(pipeline), descriptor);
    pipeline
}

pub unsafe fn render_pipeline_create_with(
    device: i32,
    descriptor: &GpuRenderPipelineDescriptor,
) -> i32 {
    converted(descriptor.wire()).map_or(0, |d| create_render_pipeline(device, &d))
}

pub unsafe fn pipeline_begin(device: i32) -> i32 {
    let mut s = state();
    if s.devices.get(device).is_none() {
        return 0;
    }
    s.builders.put(Mutex::new(Build {
        device,
        ..Default::default()
    }))
}

pub unsafe fn builder_destroy(builder: i32) {
    state().builders.remove(builder);
}

/// Run `body` on a builder, or do nothing.
fn building(builder: i32, body: impl FnOnce(&mut Build)) {
    let Some(entry) = state().builders.get(builder) else {
        return;
    };
    body(&mut entry.lock().unwrap());
}

pub unsafe fn pipeline_shader(builder: i32, shader: i32, vs: Text, fs: Text) {
    let (vs, fs) = (vs.as_str().to_owned(), fs.as_str().to_owned());
    building(builder, |b| {
        b.shader = shader;
        b.vertex_entry = vs;
        b.fragment_entry = fs;
    });
}

pub unsafe fn pipeline_layout(builder: i32, layout: i32) {
    building(builder, |b| b.layout = layout);
}

pub unsafe fn pipeline_vertex_buffer(builder: i32, stride: i64, step: i32) {
    building(builder, |b| {
        b.buffers.push(wire::GPUVertexBufferLayout {
            array_stride: stride.max(0) as u64,
            step_mode: wire::GPUVertexStepMode::from_index(step as u32),
            attributes: Vec::new(),
        })
    });
}

pub unsafe fn pipeline_attribute(builder: i32, format: i32, offset: i64, location: i32) {
    let Some(format) = wire::GPUVertexFormat::from_index(format as u32) else {
        return;
    };
    building(builder, |b| {
        if let Some(buffer) = b.buffers.last_mut() {
            buffer.attributes.push(wire::GPUVertexAttribute {
                format,
                offset: offset.max(0) as u64,
                shader_location: location.max(0) as u32,
            });
        }
    });
}

/// Packed against the previous attribute, at the next free location:
/// locations count across the pipeline's buffers, offsets within each.
pub unsafe fn pipeline_attribute_packed(builder: i32, format: i32) {
    let Some(format) = wire::GPUVertexFormat::from_index(format as u32) else {
        return;
    };
    building(builder, |b| {
        let location = b.buffers.iter().map(|l| l.attributes.len()).sum::<usize>() as u32;
        if let Some(buffer) = b.buffers.last_mut() {
            let offset = buffer
                .attributes
                .last()
                .map_or(0, |a| a.offset + vertex_size(a.format));
            buffer.attributes.push(wire::GPUVertexAttribute {
                format,
                offset,
                shader_location: location,
            });
        }
    });
}

/// A vertex format's size in bytes, from its name: `Float32x3` is three
/// 32-bit components, `Unorm1010102` one word.
fn vertex_size(format: wire::GPUVertexFormat) -> u64 {
    let name = format!("{format:?}");
    if name.contains("1010102") {
        return 4;
    }
    let digits = |s: &str| {
        s.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
    };
    let start = name
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(name.len());
    let bits: u64 = digits(&name[start..]).parse().unwrap_or(32);
    let count: u64 = name
        .find('x')
        .map_or(1, |x| digits(&name[x + 1..]).parse().unwrap_or(1));
    bits / 8 * count
}

pub unsafe fn pipeline_target(builder: i32, format: i32, write_mask: i32) {
    let Some(format) = wire::GPUTextureFormat::from_index(format as u32) else {
        return;
    };
    building(builder, |b| {
        b.targets.push(wire::GPUColorTargetState {
            format,
            blend: None,
            write_mask: Some(write_mask as u32),
        })
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
    let component = |src: i32, dst: i32, op: i32| wire::GPUBlendComponent {
        operation: wire::GPUBlendOperation::from_index(op as u32),
        src_factor: wire::GPUBlendFactor::from_index(src as u32),
        dst_factor: wire::GPUBlendFactor::from_index(dst as u32),
    };
    building(builder, |b| {
        if let Some(target) = b.targets.last_mut() {
            target.blend = Some(wire::GPUBlendState {
                color: component(src, dst, op),
                alpha: component(src_alpha, dst_alpha, op_alpha),
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
    // Both faces the same, as the native builder has them.
    let face = wire::GPUStencilFaceState {
        compare: wire::GPUCompareFunction::from_index(compare as u32),
        fail_op: wire::GPUStencilOperation::from_index(fail as u32),
        depth_fail_op: wire::GPUStencilOperation::from_index(depth_fail as u32),
        pass_op: wire::GPUStencilOperation::from_index(pass_op as u32),
    };
    building(builder, |b| {
        b.stencil = Some((face, read_mask as u32, write_mask as u32))
    });
}

pub unsafe fn pipeline_depth(builder: i32, format: i32, write: bool, compare: i32) {
    let Some(format) = wire::GPUTextureFormat::from_index(format as u32) else {
        return;
    };
    building(builder, |b| {
        b.depth = Some(wire::GPUDepthStencilState {
            format,
            depth_write_enabled: Some(write),
            depth_compare: wire::GPUCompareFunction::from_index(compare as u32),
            stencil_front: None,
            stencil_back: None,
            stencil_read_mask: None,
            stencil_write_mask: None,
            depth_bias: None,
            depth_bias_slope_scale: None,
            depth_bias_clamp: None,
        })
    });
}

pub unsafe fn pipeline_primitive(builder: i32, topology: i32, cull: i32, front: i32) {
    building(builder, |b| {
        b.primitive = Some(wire::GPUPrimitiveState {
            topology: wire::GPUPrimitiveTopology::from_index(topology as u32),
            strip_index_format: None,
            front_face: wire::GPUFrontFace::from_index(front as u32),
            cull_mode: wire::GPUCullMode::from_index(cull as u32),
            unclipped_depth: None,
        })
    });
}

pub unsafe fn render_pipeline_build(builder: i32) -> i32 {
    let Some(entry) = state().builders.get(builder) else {
        return 0;
    };
    let build = std::mem::take(&mut *entry.lock().unwrap());
    state().builders.remove(builder);
    let layout = match build.layout {
        0 => wire::GPUPipelineLayoutOrGPUAutoLayoutMode::GPUAutoLayoutMode(
            wire::GPUAutoLayoutMode::Auto,
        ),
        layout => wire::GPUPipelineLayoutOrGPUAutoLayoutMode::GPUPipelineLayout(handle(layout)),
    };
    // A stride of zero is as wide as the attributes turned out to be.
    let buffers = build
        .buffers
        .into_iter()
        .map(|mut l| {
            if l.array_stride == 0 {
                l.array_stride = l
                    .attributes
                    .iter()
                    .map(|a| a.offset + vertex_size(a.format))
                    .max()
                    .unwrap_or(0);
            }
            Some(l)
        })
        .collect();
    let depth_stencil = build.depth.map(|mut d| {
        if let Some((face, read, write)) = build.stencil {
            d.stencil_front = Some(face.clone());
            d.stencil_back = Some(face);
            d.stencil_read_mask = Some(read);
            d.stencil_write_mask = Some(write);
        }
        d
    });
    create_render_pipeline(
        build.device,
        &wire::GPURenderPipelineDescriptor {
            label: None,
            layout,
            vertex: wire::GPUVertexState {
                module: handle(build.shader),
                entry_point: Some(build.vertex_entry),
                constants: None,
                buffers: Some(buffers),
            },
            primitive: build.primitive,
            depth_stencil,
            multisample: None,
            fragment: Some(wire::GPUFragmentState {
                module: handle(build.shader),
                entry_point: Some(build.fragment_entry),
                constants: None,
                targets: build.targets.into_iter().map(Some).collect(),
            }),
        },
    )
}

// -- validity ---------------------------------------------------------------

/// Whether `h` is a live handle of the kind it carries.
pub unsafe fn is_valid(h: i32) -> bool {
    let s = state();
    let k = kind_of(h);
    let is = |kind: Kind| k == kind as i32;
    if is(Kind::Instance) {
        s.instances.get(h).is_some()
    } else if is(Kind::Adapter) {
        s.adapters.get(h).is_some()
    } else if is(Kind::Device) {
        s.devices.get(h).is_some()
    } else if is(Kind::Queue) {
        s.queues.get(h).is_some()
    } else if is(Kind::Buffer) {
        s.buffers.get(h).is_some()
    } else if is(Kind::Shader) {
        s.shaders.get(h).is_some()
    } else if is(Kind::Pipeline) {
        s.pipelines.get(h).is_some()
    } else if is(Kind::Renderpipeline) {
        s.render_pipelines.get(h).is_some()
    } else if is(Kind::BindGroupLayout) {
        s.layouts.get(h).is_some()
    } else if is(Kind::Bindgroup) {
        s.bind_groups.get(h).is_some()
    } else if is(Kind::Encoder) {
        s.encoders.get(h).is_some()
    } else if is(Kind::Bindings) {
        s.bindings.get(h).is_some()
    } else if is(Kind::Texture) {
        s.textures.get(h).is_some()
    } else if is(Kind::View) {
        s.views.get(h).is_some()
    } else if is(Kind::Surface) {
        s.surfaces.get(h).is_some()
    } else if is(Kind::Builder) {
        s.builders.get(h).is_some()
    } else {
        false
    }
}
