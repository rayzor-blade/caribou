//! Render bundles. wgpu's bundle encoder borrows its descriptor and is not
//! Send, so the commands are recorded here and replayed into one by
//! `finish`.

use std::ops::Range;
use std::sync::Arc;

use super::render::index_format;
use super::*;
use crate::GpuRenderBundleEncoderDescriptor;

enum Command {
    Pipeline(Arc<wgpu::RenderPipeline>),
    BindGroup(u32, Arc<wgpu::BindGroup>, Vec<u32>),
    Vertex(u32, Arc<wgpu::Buffer>, u64, Option<u64>),
    Index(Arc<wgpu::Buffer>, wgpu::IndexFormat, u64, Option<u64>),
    Draw(Range<u32>, Range<u32>),
    DrawIndexed(Range<u32>, i32, Range<u32>),
    DrawIndirect(Arc<wgpu::Buffer>, u64),
    DrawIndexedIndirect(Arc<wgpu::Buffer>, u64),
}

pub struct Recording {
    device: wgpu::Device,
    label: Option<String>,
    color_formats: Vec<Option<wgpu::TextureFormat>>,
    depth_stencil: Option<wgpu::RenderBundleDepthStencil>,
    sample_count: u32,
    commands: Vec<Command>,
}

pub unsafe fn bundle_encoder_create(
    device: i32,
    descriptor: &GpuRenderBundleEncoderDescriptor,
) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let sample_count = match index(descriptor.sampleCount.unwrap_or(1), "sample count") {
        Ok(count) => count,
        Err(message) => return refuse(&message),
    };
    BUNDLE_ENCODERS.lock().unwrap().put(Mutex::new(Recording {
        device: entry.device.clone(),
        label: descriptor
            .label
            .as_ref()
            .map(|text| text.get().as_str().to_owned()),
        color_formats: descriptor
            .colorFormats
            .iter()
            .map(|format| format.map(texture_format))
            .collect(),
        depth_stencil: descriptor
            .depthStencilFormat
            .map(|format| wgpu::RenderBundleDepthStencil {
                format: texture_format(format),
                depth_read_only: descriptor.depthReadOnly.unwrap_or(false),
                stencil_read_only: descriptor.stencilReadOnly.unwrap_or(false),
            }),
        sample_count,
        commands: Vec::new(),
    }))
}

pub unsafe fn bundle_encoder_destroy(encoder: i32) {
    BUNDLE_ENCODERS.lock().unwrap().remove(encoder);
}

fn record(encoder: i32, command: Command) {
    if let Some(recording) = BUNDLE_ENCODERS.lock().unwrap().get(encoder) {
        recording.lock().unwrap().commands.push(command);
    }
}

fn counted(value: i32, what: &str) -> Option<u32> {
    match index(value, what) {
        Ok(value) => Some(value),
        Err(message) => {
            host::raise(ErrorKind::Type, &message);
            None
        }
    }
}

fn range(offset: i64, size_of: i64) -> Option<(u64, Option<u64>)> {
    let Ok(start) = size(offset, "offset") else {
        host::raise(ErrorKind::Type, "negative buffer offset");
        return None;
    };
    Some((start, (size_of >= 0).then_some(size_of as u64)))
}

pub unsafe fn bundle_set_pipeline(encoder: i32, pipeline: i32) {
    let Some(pipeline) = RENDER_PIPELINES.lock().unwrap().get(pipeline) else {
        return host::raise(ErrorKind::Type, "a bundle takes a live render pipeline");
    };
    record(encoder, Command::Pipeline(pipeline));
}

pub unsafe fn bundle_set_bind_group(encoder: i32, group: i32, bindgroup: i32) {
    let bind_group = find!(BINDGROUPS, bindgroup);
    let Some(group) = counted(group, "bind group index") else {
        return;
    };
    record(encoder, Command::BindGroup(group, bind_group, Vec::new()));
}

pub unsafe fn bundle_set_bind_group_offsets(
    encoder: i32,
    group: i32,
    bindgroup: i32,
    offsets: Buffer,
    start: i64,
    count: i32,
) {
    let bind_group = find!(BINDGROUPS, bindgroup);
    let Some(group) = counted(group, "bind group index") else {
        return;
    };
    let Some(offsets) = dynamic_offsets(&offsets, start, count) else {
        return;
    };
    record(encoder, Command::BindGroup(group, bind_group, offsets));
}

pub unsafe fn bundle_set_vertex_buffer(
    encoder: i32,
    slot: i32,
    buffer: i32,
    offset: i64,
    size_of: i64,
) {
    let buffer = find!(BUFFERS, buffer);
    let (Some(slot), Some((start, len))) =
        (counted(slot, "vertex buffer slot"), range(offset, size_of))
    else {
        return;
    };
    record(encoder, Command::Vertex(slot, buffer, start, len));
}

pub unsafe fn bundle_set_index_buffer(
    encoder: i32,
    buffer: i32,
    format: i32,
    offset: i64,
    size_of: i64,
) {
    let buffer = find!(BUFFERS, buffer);
    let Some((start, len)) = range(offset, size_of) else {
        return;
    };
    record(
        encoder,
        Command::Index(buffer, index_format(format), start, len),
    );
}

pub unsafe fn bundle_draw(
    encoder: i32,
    vertex_count: i32,
    instance_count: i32,
    first_vertex: i32,
    first_instance: i32,
) {
    let (Some(vertices), Some(instances), Some(first), Some(first_instance)) = (
        counted(vertex_count, "vertex count"),
        counted(instance_count, "instance count"),
        counted(first_vertex, "first vertex"),
        counted(first_instance, "first instance"),
    ) else {
        return;
    };
    record(
        encoder,
        Command::Draw(
            first..first.saturating_add(vertices),
            first_instance..first_instance.saturating_add(instances),
        ),
    );
}

pub unsafe fn bundle_draw_indexed(
    encoder: i32,
    index_count: i32,
    instance_count: i32,
    first_index: i32,
    base_vertex: i32,
    first_instance: i32,
) {
    let (Some(indices), Some(instances), Some(first), Some(first_instance)) = (
        counted(index_count, "index count"),
        counted(instance_count, "instance count"),
        counted(first_index, "first index"),
        counted(first_instance, "first instance"),
    ) else {
        return;
    };
    record(
        encoder,
        Command::DrawIndexed(
            first..first.saturating_add(indices),
            base_vertex,
            first_instance..first_instance.saturating_add(instances),
        ),
    );
}

pub unsafe fn bundle_draw_indirect(encoder: i32, buffer: i32, offset: i64) {
    let buffer = find!(BUFFERS, buffer);
    record(encoder, Command::DrawIndirect(buffer, offset.max(0) as u64));
}

pub unsafe fn bundle_draw_indexed_indirect(encoder: i32, buffer: i32, offset: i64) {
    let buffer = find!(BUFFERS, buffer);
    record(
        encoder,
        Command::DrawIndexedIndirect(buffer, offset.max(0) as u64),
    );
}

fn slice_of(buffer: &wgpu::Buffer, start: u64, len: Option<u64>) -> wgpu::BufferSlice<'_> {
    match len {
        Some(len) => buffer.slice(start..start.saturating_add(len)),
        None => buffer.slice(start..),
    }
}

/// Replays what was recorded into wgpu's encoder. Consumes the encoder.
pub unsafe fn bundle_finish(encoder: i32) -> i32 {
    let Some(entry) = BUNDLE_ENCODERS.lock().unwrap().get(encoder) else {
        return 0;
    };
    BUNDLE_ENCODERS.lock().unwrap().remove(encoder);
    let recording = entry.lock().unwrap();
    let mut bundle =
        recording
            .device
            .create_render_bundle_encoder(&wgpu::RenderBundleEncoderDescriptor {
                label: recording.label.as_deref(),
                color_formats: &recording.color_formats,
                depth_stencil: recording.depth_stencil,
                sample_count: recording.sample_count,
                multiview: None,
            });
    for command in &recording.commands {
        match command {
            Command::Pipeline(pipeline) => bundle.set_pipeline(pipeline),
            Command::BindGroup(group, bind_group, offsets) => {
                bundle.set_bind_group(*group, &**bind_group, offsets)
            }
            Command::Vertex(slot, buffer, start, len) => {
                bundle.set_vertex_buffer(*slot, slice_of(buffer, *start, *len))
            }
            Command::Index(buffer, format, start, len) => {
                bundle.set_index_buffer(slice_of(buffer, *start, *len), *format)
            }
            Command::Draw(vertices, instances) => bundle.draw(vertices.clone(), instances.clone()),
            Command::DrawIndexed(indices, base, instances) => {
                bundle.draw_indexed(indices.clone(), *base, instances.clone())
            }
            Command::DrawIndirect(buffer, offset) => bundle.draw_indirect(buffer, *offset),
            Command::DrawIndexedIndirect(buffer, offset) => {
                bundle.draw_indexed_indirect(buffer, *offset)
            }
        }
    }
    let finished = bundle.finish(&wgpu::RenderBundleDescriptor {
        label: recording.label.as_deref(),
    });
    BUNDLES.lock().unwrap().put(finished)
}

pub unsafe fn bundle_destroy(bundle: i32) {
    BUNDLES.lock().unwrap().remove(bundle);
}
