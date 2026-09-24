//! Error scopes, device loss and the compiler's structured messages.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use super::*;

pub struct ErrorEntry {
    filter: i32,
    message: String,
}

pub struct LostEntry {
    reason: i32,
    message: String,
}

pub struct Message {
    text: String,
    kind: i32,
    line: u64,
    column: u64,
    offset: u64,
    length: u64,
}

thread_local! {
    /// wgpu keeps error scopes per device and per thread; so do these, as
    /// the guards wgpu hands back cannot leave the thread.
    static SCOPES: RefCell<Vec<(i32, wgpu::ErrorScopeGuard)>> = const { RefCell::new(Vec::new()) };
}

// WebGPU's filter order: validation, out-of-memory, internal.
pub unsafe fn error_scope_push(device: i32, filter: i32) {
    let entry = find!(DEVICES, device);
    let filter = match filter {
        1 => wgpu::ErrorFilter::OutOfMemory,
        2 => wgpu::ErrorFilter::Internal,
        _ => wgpu::ErrorFilter::Validation,
    };
    let guard = entry.device.push_error_scope(filter);
    SCOPES.with(|scopes| scopes.borrow_mut().push((device, guard)));
}

pub unsafe fn error_scope_pop(device: i32) -> Future<crate::GpuError> {
    let guard = SCOPES.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        let at = scopes.iter().rposition(|(owner, _)| *owner == device)?;
        Some(scopes.remove(at).1)
    });
    let Some(guard) = guard else {
        return rejected_future("no error scope was pushed for this device on this thread");
    };
    let popped = guard.pop();
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        let Some(error) = popped.await else {
            completion.get().resolve(caribou_abi::Value::null());
            return;
        };
        let filter = match error {
            wgpu::Error::OutOfMemory { .. } => 1,
            wgpu::Error::Internal { .. } => 2,
            _ => 0,
        };
        let handle = ERRORS.lock().unwrap().put(ErrorEntry {
            filter,
            message: error_text(&error),
        });
        if !completion
            .get()
            .resolve_boxed(Box::new(crate::GpuError { handle }))
        {
            ERRORS.lock().unwrap().remove(handle);
        }
    });
    future
}

pub unsafe fn error_destroy(error: i32) {
    ERRORS.lock().unwrap().remove(error);
}

pub unsafe fn error_filter(error: i32) -> i32 {
    find!(ERRORS, error, 0).filter
}

pub unsafe fn error_message(error: i32) -> Text {
    Text::new(&find!(ERRORS, error, Text::NULL).message)
}

/// Whether a device is lost, and who is waiting to hear.
#[derive(Default)]
pub struct Lost {
    reason: Option<(i32, String)>,
    waiting: Vec<Rooted<Future<crate::GpuDeviceLostInfo>>>,
}

fn announce(completion: &Rooted<Future<crate::GpuDeviceLostInfo>>, reason: i32, message: &str) {
    let handle = LOST_INFOS.lock().unwrap().put(LostEntry {
        reason,
        message: message.to_owned(),
    });
    if !completion
        .get()
        .resolve_boxed(Box::new(crate::GpuDeviceLostInfo { handle }))
    {
        LOST_INFOS.lock().unwrap().remove(handle);
    }
}

/// What a new device reports when it is lost, destroyed included.
pub(super) fn watch(device: &wgpu::Device) -> Arc<Mutex<Lost>> {
    let lost = Arc::new(Mutex::new(Lost::default()));
    let reported = lost.clone();
    device.set_device_lost_callback(move |reason, message| {
        let reason = match reason {
            wgpu::DeviceLostReason::Destroyed => 1,
            _ => 0,
        };
        let waiting = {
            let mut lost = reported.lock().unwrap();
            lost.reason = Some((reason, message.clone()));
            std::mem::take(&mut lost.waiting)
        };
        for completion in &waiting {
            announce(completion, reason, &message);
        }
    });
    lost
}

pub unsafe fn device_lost(device: i32) -> Future<crate::GpuDeviceLostInfo> {
    let Some(entry) = DEVICES.lock().unwrap().get(device) else {
        return rejected_future("device was destroyed");
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    let mut lost = entry.lost.lock().unwrap();
    match &lost.reason {
        Some((reason, message)) => announce(&completion, *reason, message),
        None => lost.waiting.push(completion),
    }
    future
}

pub unsafe fn lost_destroy(info: i32) {
    LOST_INFOS.lock().unwrap().remove(info);
}

pub unsafe fn lost_reason(info: i32) -> i32 {
    find!(LOST_INFOS, info, 0).reason
}

pub unsafe fn lost_message(info: i32) -> Text {
    Text::new(&find!(LOST_INFOS, info, Text::NULL).message)
}

pub unsafe fn shader_compilation_info(shader: i32) -> Future<crate::GpuCompilationInfo> {
    let Some(module) = SHADERS.lock().unwrap().get(shader) else {
        return rejected_future("shader module was destroyed");
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        let info = module.get_compilation_info().await;
        let messages: Vec<Message> = info
            .messages
            .into_iter()
            .map(|m| {
                let (line, column, offset, length) = m.location.map_or((0, 0, 0, 0), |at| {
                    (at.line_number, at.line_position, at.offset, at.length)
                });
                Message {
                    kind: match m.message_type {
                        wgpu::CompilationMessageType::Error => 0,
                        wgpu::CompilationMessageType::Warning => 1,
                        wgpu::CompilationMessageType::Info => 2,
                    },
                    text: m.message,
                    line: u64::from(line),
                    column: u64::from(column),
                    offset: u64::from(offset),
                    length: u64::from(length),
                }
            })
            .collect();
        let handle = COMPILATIONS.lock().unwrap().put(messages);
        if !completion
            .get()
            .resolve_boxed(Box::new(crate::GpuCompilationInfo { handle }))
        {
            COMPILATIONS.lock().unwrap().remove(handle);
        }
    });
    future
}

pub unsafe fn compilation_destroy(info: i32) {
    COMPILATIONS.lock().unwrap().remove(info);
}

pub unsafe fn compilation_count(info: i32) -> i32 {
    find!(COMPILATIONS, info, 0).len() as i32
}

/// The `index`th message's value, or a raised type error out of range.
fn message<T: Default>(info: i32, index_of: i32, read: impl FnOnce(&Message) -> T) -> T {
    let Some(messages) = COMPILATIONS.lock().unwrap().get(info) else {
        return T::default();
    };
    match usize::try_from(index_of).ok().and_then(|i| messages.get(i)) {
        Some(found) => read(found),
        None => {
            host::raise(ErrorKind::Type, "compilation message index out of range");
            T::default()
        }
    }
}

pub unsafe fn compilation_message(info: i32, index_of: i32) -> Text {
    match message(info, index_of, |m| Some(m.text.clone())) {
        Some(text) => Text::new(&text),
        None => Text::NULL,
    }
}

pub unsafe fn compilation_type(info: i32, index_of: i32) -> i32 {
    message(info, index_of, |m| m.kind)
}

pub unsafe fn compilation_line(info: i32, index_of: i32) -> i64 {
    message(info, index_of, |m| m.line as i64)
}

pub unsafe fn compilation_column(info: i32, index_of: i32) -> i64 {
    message(info, index_of, |m| m.column as i64)
}

pub unsafe fn compilation_offset(info: i32, index_of: i32) -> i64 {
    message(info, index_of, |m| m.offset as i64)
}

pub unsafe fn compilation_length(info: i32, index_of: i32) -> i64 {
    message(info, index_of, |m| m.length as i64)
}
