//! Pipeline caches, wgpu's own with PIPELINE_CACHE: what compiling
//! pipelines produced, saved and loaded back on a later run.

use std::sync::Arc;

use super::*;
use crate::GpuPipelineCacheDescriptor;

pub unsafe fn pipeline_cache_create(device: i32, d: &GpuPipelineCacheDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    if d.data.is_some() && !entry.cache_data {
        return refuse(
            "loading pipeline cache data needs pipelineCacheData(true) on the device descriptor",
        );
    }
    let data = d.data.as_ref().map(|b| {
        let buffer = b.get();
        unsafe { buffer.as_slice() }.to_vec()
    });
    let label = d.label.as_ref().map(caribou_abi::Rooted::get);
    // Without data there is nothing to trust. With it, the device's
    // pipelineCacheData: the program vouches that the bytes are ones
    // getData returned.
    let cache = unsafe {
        entry
            .device
            .create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: label.as_ref().map(Text::as_str),
                data: data.as_deref(),
                fallback: d.fallback.unwrap_or(true),
            })
    };
    PIPELINE_CACHES.lock().unwrap().put(cache)
}

pub unsafe fn pipeline_cache_destroy(cache: i32) {
    PIPELINE_CACHES.lock().unwrap().remove(cache);
}

pub unsafe fn pipeline_cache_data(cache: i32) -> Buffer {
    let cache = find!(PIPELINE_CACHES, cache, Buffer::NULL);
    match cache.get_data() {
        Some(data) => Buffer::new(&data),
        None => Buffer::NULL,
    }
}

pub unsafe fn adapter_pipeline_cache_key(adapter: i32) -> Text {
    let adapter = find!(ADAPTERS, adapter, Text::NULL);
    match wgpu::util::pipeline_cache_key(&adapter.get_info()) {
        Some(key) => Text::new(&key),
        None => Text::NULL,
    }
}

/// A pipeline descriptor's cache, if it names one.
pub(super) fn pipeline_cache(
    handle: Option<i32>,
) -> Result<Option<Arc<wgpu::PipelineCache>>, String> {
    handle
        .map(|handle| {
            PIPELINE_CACHES
                .lock()
                .unwrap()
                .get(handle)
                .ok_or_else(|| "the pipeline cache was destroyed".to_owned())
        })
        .transpose()
}
