//! The GPU plugin in a browser: each backend function a program calls is
//! encoded onto the wire to the page's WebGPU (`crate::wire`), which the
//! GPU agent decodes and runs. Functions this module does not define raise
//! that they are not available on the web (the generated `backend`).
//!
//! Commands collect in one batch, handed to the agent through the mailbox
//! when the program needs something to happen: a request, an upload, a
//! submit, a read. Objects are kept by the agent under the plugin's own
//! handles, so making one needs no round trip; the GPU itself is handle 1,
//! which no plugin handle is. A promise settles its future from a thread
//! that waits for the agent to settle replies.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering::SeqCst};
use std::sync::{LazyLock, Mutex, Once};

use caribou_abi::{Buffer, BufferMut, ErrorKind, Future, Rooted, Text, Value, host};

use crate::handles::Slab;
use crate::types::Kind;
use crate::wire::{self, Handle, Mailbox};
use crate::{
    GpuBindGroupDescriptor, GpuBufferDescriptor, GpuComputePipelineDescriptor, GpuDeviceDescriptor,
    GpuRequestAdapterOptions, GpuShaderModuleDescriptor,
};

/// `navigator.gpu`, as the agent keeps it.
const GPU: Handle = Handle(1);

/// Handles of objects the plugin makes and releases in one batch (passes,
/// command buffers, layouts it only borrows), below every plugin handle.
const TRANSIENT: u32 = 1 << 26;

static MAILBOX: Mailbox = Mailbox::new();

/// A buffer's size and usage, which WebGPU keeps as attributes; held here
/// so reading them needs no round trip.
struct BufferEntry {
    size: i64,
    usage: i32,
}

struct State {
    commands: wire::Encoder,
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
    layouts: Slab<()>,
    bind_groups: Slab<()>,
    /// Each encoder's open compute pass, or zero.
    encoders: Slab<AtomicU32>,
    bindings: Slab<Mutex<Vec<i32>>>,
    pending: Vec<Pending>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        commands: wire::Encoder::new(),
        agent: false,
        next: 1,
        instances: Slab::new(Kind::Instance),
        adapters: Slab::new(Kind::Adapter),
        devices: Slab::new(Kind::Device),
        queues: Slab::new(Kind::Queue),
        buffers: Slab::new(Kind::Buffer),
        shaders: Slab::new(Kind::Shader),
        pipelines: Slab::new(Kind::Pipeline),
        layouts: Slab::new(Kind::BindGroupLayout),
        bind_groups: Slab::new(Kind::Bindgroup),
        encoders: Slab::new(Kind::Encoder),
        bindings: Slab::new(Kind::Bindings),
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
    }

    fn transient(&mut self) -> Handle {
        self.next = if self.next + 1 >= TRANSIENT {
            2
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

/// Wait for `promise` in the background, settling `waiting` from it.
fn wait(s: &mut State, promise: Box<Promise>, waiting: Waiting) {
    s.pending.push(Pending { promise, waiting });
    static WATCH: Once = Once::new();
    WATCH.call_once(|| {
        std::thread::spawn(|| {
            let mut seen = 0;
            loop {
                seen = MAILBOX.wait_settled(seen);
                settle();
            }
        });
    });
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

/// `len` bytes of `data`, checked before the agent reads them.
fn bytes(data: &Buffer, len: i32) -> Option<wire::Bytes> {
    let Ok(len) = usize::try_from(len) else {
        host::raise(ErrorKind::Type, "negative byte length");
        return None;
    };
    if len > data.len() {
        host::raise(ErrorKind::Type, "byte length exceeds shared buffer");
        return None;
    }
    Some(wire::Bytes {
        address: data.as_ptr() as usize as u32,
        len: len as u32,
    })
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
    s.flush();
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
    if data.len == 0 {
        return;
    }
    let mut s = state();
    if s.queues.get(queue).is_none() || s.buffers.get(buffer).is_none() {
        return;
    }
    s.commands.gpu_queue_write_buffer(
        handle(queue),
        &handle(buffer),
        &(offset.max(0) as u64),
        &data,
        &None,
        &None,
    );
    // The agent reads the bytes where they are, so before they can change.
    s.flush();
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
    if bytes(&out.buffer(), len).is_none() || len <= 0 {
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

pub unsafe fn pipeline_destroy(pipeline: i32) {
    forget(|s| s.pipelines.remove(pipeline), pipeline);
}

pub unsafe fn pipeline_bind_group_layout(pipeline: i32, index: i32) -> i32 {
    let mut s = state();
    if s.pipelines.get(pipeline).is_none() {
        return 0;
    }
    let layout = s.layouts.put(());
    s.commands.gpu_compute_pipeline_get_bind_group_layout(
        handle(pipeline),
        handle(layout),
        &(index.max(0) as u32),
    );
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
    if s.devices.get(device).is_none() || s.pipelines.get(pipeline).is_none() {
        return 0;
    }
    let entries = list
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
    let bind_group = s.bind_groups.put(());
    s.commands.gpu_compute_pipeline_get_bind_group_layout(
        handle(pipeline),
        layout,
        &(group.max(0) as u32),
    );
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
    let encoder = s.encoders.put(AtomicU32::new(0));
    s.commands
        .gpu_device_create_command_encoder(handle(device), handle(encoder), &None);
    encoder
}

pub unsafe fn encoder_destroy(encoder: i32) {
    forget(|s| s.encoders.remove(encoder), encoder);
}

/// The compute pass open on `encoder`.
fn pass(s: &State, encoder: i32) -> Option<Handle> {
    let open = s.encoders.get(encoder)?.load(SeqCst);
    (open != 0).then_some(Handle(open))
}

pub unsafe fn compute_begin(encoder: i32) {
    let mut s = state();
    let Some(entry) = s.encoders.get(encoder) else {
        return;
    };
    if entry.load(SeqCst) != 0 {
        host::raise(
            ErrorKind::Runtime,
            "gpu: a compute pass is already open on this encoder",
        );
        return;
    }
    let pass = s.transient();
    s.commands
        .gpu_command_encoder_begin_compute_pass(handle(encoder), pass, &None);
    entry.store(pass.0, SeqCst);
}

pub unsafe fn compute_set_pipeline(encoder: i32, pipeline: i32) {
    let mut s = state();
    if let Some(pass) = pass(&s, encoder) {
        s.commands
            .gpu_compute_pass_encoder_set_pipeline(pass, &handle(pipeline));
    }
}

pub unsafe fn compute_set_bind_group(encoder: i32, group: i32, bind_group: i32) {
    let mut s = state();
    if let Some(pass) = pass(&s, encoder) {
        s.commands.gpu_compute_pass_encoder_set_bind_group(
            pass,
            &(group.max(0) as u32),
            &Some(handle(bind_group)),
            &None,
        );
    }
}

pub unsafe fn compute_dispatch(encoder: i32, x: i32, y: i32, z: i32) {
    let mut s = state();
    if let Some(pass) = pass(&s, encoder) {
        s.commands.gpu_compute_pass_encoder_dispatch_workgroups(
            pass,
            &(x.max(0) as u32),
            &Some(y.max(0) as u32),
            &Some(z.max(0) as u32),
        );
    }
}

fn end_pass(s: &mut State, encoder: i32) {
    if let Some(pass) = pass(s, encoder) {
        s.commands.gpu_compute_pass_encoder_end(pass);
        s.commands.release(pass);
        if let Some(entry) = s.encoders.get(encoder) {
            entry.store(0, SeqCst);
        }
    }
}

pub unsafe fn compute_end(encoder: i32) {
    end_pass(&mut state(), encoder);
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
    let mut s = state();
    if s.encoders.get(encoder).is_none()
        || s.buffers.get(src).is_none()
        || s.buffers.get(dst).is_none()
    {
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
}

/// Finish the encoder and submit it; it is spent either way.
pub unsafe fn encoder_submit(encoder: i32, queue: i32) {
    let mut s = state();
    if s.encoders.get(encoder).is_none() {
        return;
    }
    end_pass(&mut s, encoder);
    s.encoders.remove(encoder);
    if s.queues.get(queue).is_some() {
        let commands = s.transient();
        s.commands
            .gpu_command_encoder_finish(handle(encoder), commands, &None);
        s.commands.gpu_queue_submit(handle(queue), &vec![commands]);
        s.commands.release(commands);
    }
    s.commands.release(handle(encoder));
    s.flush();
}
