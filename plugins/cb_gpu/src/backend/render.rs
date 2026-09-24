//! Pipelines from WebGPU's descriptors, passes from theirs, and the draw,
//! immediate and query commands recorded inside passes.

use std::num::NonZeroU32;
use std::sync::Arc;

use super::*;
use crate::{
    AttachmentView, GpuBlendComponent, GpuColorTargetState, GpuComputePassDescriptor,
    GpuComputePipelineDescriptor, GpuDepthStencilState, GpuMultisampleState, GpuPrimitiveState,
    GpuRenderPassDescriptor, GpuRenderPipelineDescriptor, GpuStencilFaceState,
};

fn raise(message: &str) {
    host::raise(ErrorKind::Type, message);
}

/// One programmable stage, owned, so a pipeline can be built on a worker.
struct Stage {
    module: Arc<wgpu::ShaderModule>,
    entry: Option<String>,
    constants: Vec<(String, f64)>,
}

impl Stage {
    fn of(
        module: i32,
        entry: &Option<caribou_abi::Rooted<Text>>,
        constants: &[(caribou_abi::Rooted<Text>, f64)],
    ) -> Result<Stage, String> {
        Ok(Stage {
            module: SHADERS
                .lock()
                .unwrap()
                .get(module)
                .ok_or("the shader module was destroyed")?,
            entry: entry.as_ref().map(|name| name.get().as_str().to_owned()),
            constants: constants
                .iter()
                .map(|(name, value)| (name.get().as_str().to_owned(), *value))
                .collect(),
        })
    }

    fn pairs(&self) -> Vec<(&str, f64)> {
        self.constants
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
            .collect()
    }
}

fn options<'a>(constants: &'a [(&'a str, f64)]) -> wgpu::PipelineCompilationOptions<'a> {
    wgpu::PipelineCompilationOptions {
        constants,
        ..Default::default()
    }
}

fn pipeline_layout(handle: Option<i32>) -> Result<Option<Arc<wgpu::PipelineLayout>>, String> {
    // Unset is WebGPU's "auto": the layout is inferred from the shaders.
    handle
        .map(|handle| {
            PIPELINE_LAYOUTS
                .lock()
                .unwrap()
                .get(handle)
                .ok_or_else(|| "the pipeline layout was destroyed".to_owned())
        })
        .transpose()
}

fn label(label: &Option<caribou_abi::Rooted<Text>>) -> Option<String> {
    label.as_ref().map(|text| text.get().as_str().to_owned())
}

// -- compute pipelines ---------------------------------------------------------------

struct ComputePlan {
    device: wgpu::Device,
    label: Option<String>,
    layout: Option<Arc<wgpu::PipelineLayout>>,
    stage: Stage,
}

fn compute_plan(device: i32, d: &GpuComputePipelineDescriptor) -> Result<ComputePlan, String> {
    let entry = DEVICES
        .lock()
        .unwrap()
        .get(device)
        .ok_or("the device was destroyed")?;
    Ok(ComputePlan {
        device: entry.device.clone(),
        label: label(&d.label),
        layout: pipeline_layout(d.layout)?,
        stage: Stage::of(
            d.compute.module,
            &d.compute.entryPoint,
            &d.compute.constants,
        )?,
    })
}

fn build_compute(plan: &ComputePlan) -> wgpu::ComputePipeline {
    let constants = plan.stage.pairs();
    plan.device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: plan.label.as_deref(),
            layout: plan.layout.as_deref(),
            module: &plan.stage.module,
            entry_point: plan.stage.entry.as_deref(),
            compilation_options: options(&constants),
            cache: None,
        })
}

pub unsafe fn compute_pipeline_create_with(
    device: i32,
    descriptor: &GpuComputePipelineDescriptor,
) -> i32 {
    match compute_plan(device, descriptor) {
        Ok(plan) => PIPELINES.lock().unwrap().put(build_compute(&plan)),
        Err(message) => refuse(&message),
    }
}

/// Built on a worker, so shader compilation does not hold up the caller.
pub unsafe fn compute_pipeline_create_async(
    device: i32,
    descriptor: &GpuComputePipelineDescriptor,
) -> Future<crate::GpuPipeline> {
    let plan = match compute_plan(device, descriptor) {
        Ok(plan) => plan,
        Err(message) => return rejected_future(&message),
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        let handle = PIPELINES.lock().unwrap().put(build_compute(&plan));
        settle_pipeline(&completion, handle);
    });
    future
}

fn settle_pipeline(completion: &Rooted<Future<crate::GpuPipeline>>, handle: i32) {
    if handle == 0 {
        completion
            .get()
            .reject(Text::new("pipeline resource table is full").value());
    } else if !completion
        .get()
        .resolve_boxed(Box::new(crate::GpuPipeline { handle }))
    {
        unsafe { pipeline_release(handle) };
    }
}

// -- render pipelines ----------------------------------------------------------------

type VertexBuffer = (u64, wgpu::VertexStepMode, Vec<wgpu::VertexAttribute>);

struct RenderPlan {
    device: wgpu::Device,
    label: Option<String>,
    layout: Option<Arc<wgpu::PipelineLayout>>,
    vertex: Stage,
    buffers: Vec<Option<VertexBuffer>>,
    primitive: wgpu::PrimitiveState,
    depth_stencil: Option<wgpu::DepthStencilState>,
    multisample: wgpu::MultisampleState,
    fragment: Option<(Stage, Vec<Option<wgpu::ColorTargetState>>)>,
    multiview: Option<NonZeroU32>,
}

pub(super) fn index_format(value: i32) -> wgpu::IndexFormat {
    if value == 1 {
        wgpu::IndexFormat::Uint32
    } else {
        wgpu::IndexFormat::Uint16
    }
}

// Defaults below are WebGPU's, in the IDL enums' orders.
fn primitive(p: Option<&GpuPrimitiveState>) -> wgpu::PrimitiveState {
    let Some(p) = p else {
        return wgpu::PrimitiveState::default();
    };
    wgpu::PrimitiveState {
        topology: topology(p.topology.unwrap_or(3)),
        strip_index_format: p.stripIndexFormat.map(index_format),
        front_face: front_face(p.frontFace.unwrap_or(0)),
        cull_mode: cull_mode(p.cullMode.unwrap_or(0)),
        unclipped_depth: p.unclippedDepth.unwrap_or(false),
        polygon_mode: match p.polygonMode {
            Some(1) => wgpu::PolygonMode::Line,
            Some(2) => wgpu::PolygonMode::Point,
            _ => wgpu::PolygonMode::Fill,
        },
        conservative: p.conservative.unwrap_or(false),
    }
}

fn stencil_face(face: Option<&GpuStencilFaceState>) -> wgpu::StencilFaceState {
    let Some(face) = face else {
        return wgpu::StencilFaceState::IGNORE;
    };
    wgpu::StencilFaceState {
        compare: compare_function(face.compare.unwrap_or(7)),
        fail_op: stencil_operation(face.failOp.unwrap_or(0)),
        depth_fail_op: stencil_operation(face.depthFailOp.unwrap_or(0)),
        pass_op: stencil_operation(face.passOp.unwrap_or(0)),
    }
}

fn depth_stencil(d: &GpuDepthStencilState) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: texture_format(d.format),
        depth_write_enabled: d.depthWriteEnabled,
        depth_compare: d.depthCompare.map(compare_function),
        stencil: wgpu::StencilState {
            front: stencil_face(d.stencilFront.as_ref()),
            back: stencil_face(d.stencilBack.as_ref()),
            // A mask is 32 bits; -1 in a language is every bit.
            read_mask: d.stencilReadMask.map_or(u32::MAX, |mask| mask as u32),
            write_mask: d.stencilWriteMask.map_or(u32::MAX, |mask| mask as u32),
        },
        bias: wgpu::DepthBiasState {
            constant: d.depthBias.unwrap_or(0),
            slope_scale: d.depthBiasSlopeScale.unwrap_or(0.0),
            clamp: d.depthBiasClamp.unwrap_or(0.0),
        },
    }
}

fn multisample(m: Option<&GpuMultisampleState>) -> Result<wgpu::MultisampleState, String> {
    let Some(m) = m else {
        return Ok(wgpu::MultisampleState::default());
    };
    Ok(wgpu::MultisampleState {
        count: index(m.count.unwrap_or(1), "sample count")?,
        mask: m.mask.map_or(u64::MAX, |mask| u64::from(mask as u32)),
        alpha_to_coverage_enabled: m.alphaToCoverageEnabled.unwrap_or(false),
    })
}

fn blend_component(c: &GpuBlendComponent) -> wgpu::BlendComponent {
    wgpu::BlendComponent {
        src_factor: blend_factor(c.srcFactor.unwrap_or(1)),
        dst_factor: blend_factor(c.dstFactor.unwrap_or(0)),
        operation: blend_operation(c.operation.unwrap_or(0)),
    }
}

fn color_target(t: &GpuColorTargetState) -> wgpu::ColorTargetState {
    wgpu::ColorTargetState {
        format: texture_format(t.format),
        blend: t.blend.as_ref().map(|blend| wgpu::BlendState {
            color: blend_component(&blend.color),
            alpha: blend_component(&blend.alpha),
        }),
        write_mask: wgpu::ColorWrites::from_bits_truncate(t.writeMask.unwrap_or(0xF) as u32),
    }
}

fn render_plan(device: i32, d: &GpuRenderPipelineDescriptor) -> Result<RenderPlan, String> {
    let entry = DEVICES
        .lock()
        .unwrap()
        .get(device)
        .ok_or("the device was destroyed")?;
    let mut buffers = Vec::with_capacity(d.vertex.buffers.len());
    for layout in &d.vertex.buffers {
        buffers.push(match layout {
            None => None,
            Some(layout) => {
                let mut attributes = Vec::with_capacity(layout.attributes.len());
                for a in &layout.attributes {
                    attributes.push(wgpu::VertexAttribute {
                        format: vertex_format(a.format),
                        offset: size(a.offset, "attribute offset")?,
                        shader_location: index(a.shaderLocation, "shader location")?,
                    });
                }
                Some((
                    size(layout.arrayStride, "array stride")?,
                    step_mode(layout.stepMode.unwrap_or(0)),
                    attributes,
                ))
            }
        });
    }
    let fragment = match &d.fragment {
        None => None,
        Some(f) => Some((
            Stage::of(f.module, &f.entryPoint, &f.constants)?,
            f.targets
                .iter()
                .map(|target| target.as_ref().map(color_target))
                .collect(),
        )),
    };
    let multiview = match d.multiviewMask {
        None => None,
        Some(mask) => Some(NonZeroU32::new(mask as u32).ok_or("a multiview mask of 0")?),
    };
    Ok(RenderPlan {
        device: entry.device.clone(),
        label: label(&d.label),
        layout: pipeline_layout(d.layout)?,
        vertex: Stage::of(d.vertex.module, &d.vertex.entryPoint, &d.vertex.constants)?,
        buffers,
        primitive: primitive(d.primitive.as_ref()),
        depth_stencil: d.depthStencil.as_ref().map(depth_stencil),
        multisample: multisample(d.multisample.as_ref())?,
        fragment,
        multiview,
    })
}

fn build_render(plan: &RenderPlan) -> wgpu::RenderPipeline {
    let vertex_constants = plan.vertex.pairs();
    let buffers: Vec<Option<wgpu::VertexBufferLayout>> = plan
        .buffers
        .iter()
        .map(|buffer| {
            buffer
                .as_ref()
                .map(|(stride, step, attributes)| wgpu::VertexBufferLayout {
                    array_stride: *stride,
                    step_mode: *step,
                    attributes,
                })
        })
        .collect();
    let fragment_constants = plan
        .fragment
        .as_ref()
        .map(|(stage, _)| stage.pairs())
        .unwrap_or_default();
    plan.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: plan.label.as_deref(),
            layout: plan.layout.as_deref(),
            vertex: wgpu::VertexState {
                module: &plan.vertex.module,
                entry_point: plan.vertex.entry.as_deref(),
                buffers: &buffers,
                compilation_options: options(&vertex_constants),
            },
            primitive: plan.primitive,
            depth_stencil: plan.depth_stencil.clone(),
            multisample: plan.multisample,
            fragment: plan
                .fragment
                .as_ref()
                .map(|(stage, targets)| wgpu::FragmentState {
                    module: &stage.module,
                    entry_point: stage.entry.as_deref(),
                    targets,
                    compilation_options: options(&fragment_constants),
                }),
            multiview_mask: plan.multiview,
            cache: None,
        })
}

pub unsafe fn render_pipeline_create_with(
    device: i32,
    descriptor: &GpuRenderPipelineDescriptor,
) -> i32 {
    match render_plan(device, descriptor) {
        Ok(plan) => RENDER_PIPELINES.lock().unwrap().put(build_render(&plan)),
        Err(message) => refuse(&message),
    }
}

pub unsafe fn render_pipeline_create_async(
    device: i32,
    descriptor: &GpuRenderPipelineDescriptor,
) -> Future<crate::GpuPipeline> {
    let plan = match render_plan(device, descriptor) {
        Ok(plan) => plan,
        Err(message) => return rejected_future(&message),
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        let handle = RENDER_PIPELINES.lock().unwrap().put(build_render(&plan));
        settle_pipeline(&completion, handle);
    });
    future
}

// -- passes from descriptors ------------------------------------------------------------

/// A view to render into: the view given, or a texture's default view.
fn attachment_view(view: Option<&AttachmentView>) -> Result<Arc<wgpu::TextureView>, String> {
    match view {
        None => Err("an attachment has no view".into()),
        Some(AttachmentView::TextureView(handle)) => VIEWS
            .lock()
            .unwrap()
            .get(*handle)
            .ok_or_else(|| "an attachment's texture view was destroyed".into()),
        Some(AttachmentView::Texture(handle)) => {
            let texture = TEXTURES
                .lock()
                .unwrap()
                .get(*handle)
                .ok_or("an attachment's texture was destroyed")?;
            Ok(Arc::new(texture.create_view(&Default::default())))
        }
    }
}

fn load<V>(op: i32, clear: V) -> wgpu::LoadOp<V> {
    if op == 1 {
        wgpu::LoadOp::Clear(clear)
    } else {
        wgpu::LoadOp::Load
    }
}

fn store(op: i32) -> wgpu::StoreOp {
    if op == 1 {
        wgpu::StoreOp::Discard
    } else {
        wgpu::StoreOp::Store
    }
}

fn query_set(handle: i32) -> Result<Arc<QuerySetEntry>, String> {
    QUERY_SETS
        .lock()
        .unwrap()
        .get(handle)
        .ok_or_else(|| "the query set was destroyed".into())
}

struct ColorPlan {
    view: Arc<wgpu::TextureView>,
    resolve: Option<Arc<wgpu::TextureView>>,
    depth_slice: Option<u32>,
    ops: wgpu::Operations<wgpu::Color>,
}

struct DepthPlan {
    view: Arc<wgpu::TextureView>,
    depth: Option<wgpu::Operations<f32>>,
    stencil: Option<wgpu::Operations<u32>>,
}

type Writes = (Arc<QuerySetEntry>, Option<u32>, Option<u32>);

fn timestamp_writes(set: i32, beginning: Option<i32>, end: Option<i32>) -> Result<Writes, String> {
    Ok((
        query_set(set)?,
        beginning.map(|i| index(i, "timestamp index")).transpose()?,
        end.map(|i| index(i, "timestamp index")).transpose()?,
    ))
}

/// What a render pass descriptor resolves to before the pass opens.
struct PassPlan {
    colors: Vec<Option<ColorPlan>>,
    depth: Option<DepthPlan>,
    writes: Option<Writes>,
    occlusion: Option<Arc<QuerySetEntry>>,
}

fn render_plan_pass(d: &GpuRenderPassDescriptor) -> Result<PassPlan, String> {
    let mut colors = Vec::with_capacity(d.colorAttachments.len());
    for attachment in &d.colorAttachments {
        let Some(a) = attachment else {
            colors.push(None);
            continue;
        };
        let clear = a
            .clearValue
            .as_ref()
            .map_or(wgpu::Color::TRANSPARENT, |c| wgpu::Color {
                r: c.r,
                g: c.g,
                b: c.b,
                a: c.a,
            });
        colors.push(Some(ColorPlan {
            view: attachment_view(a.view.as_ref())?,
            resolve: a
                .resolveTarget
                .as_ref()
                .map(|target| attachment_view(Some(target)))
                .transpose()?,
            depth_slice: a.depthSlice.map(|s| index(s, "depth slice")).transpose()?,
            ops: wgpu::Operations {
                load: load(a.loadOp, clear),
                store: store(a.storeOp),
            },
        }));
    }
    let depth = match &d.depthStencilAttachment {
        None => None,
        Some(a) => {
            // Read-only, or given no operations: the aspect is not written.
            let depth = (!a.depthReadOnly.unwrap_or(false)
                && (a.depthLoadOp.is_some() || a.depthStoreOp.is_some()))
            .then(|| wgpu::Operations {
                load: load(a.depthLoadOp.unwrap_or(0), a.depthClearValue.unwrap_or(0.0)),
                store: store(a.depthStoreOp.unwrap_or(0)),
            });
            let stencil = (!a.stencilReadOnly.unwrap_or(false)
                && (a.stencilLoadOp.is_some() || a.stencilStoreOp.is_some()))
            .then(|| wgpu::Operations {
                load: load(
                    a.stencilLoadOp.unwrap_or(0),
                    a.stencilClearValue.unwrap_or(0) as u32,
                ),
                store: store(a.stencilStoreOp.unwrap_or(0)),
            });
            Some(DepthPlan {
                view: attachment_view(a.view.as_ref())?,
                depth,
                stencil,
            })
        }
    };
    let writes = d
        .timestampWrites
        .as_ref()
        .map(|w| {
            timestamp_writes(
                w.querySet,
                w.beginningOfPassWriteIndex,
                w.endOfPassWriteIndex,
            )
        })
        .transpose()?;
    let occlusion = d.occlusionQuerySet.map(query_set).transpose()?;
    Ok(PassPlan {
        colors,
        depth,
        writes,
        occlusion,
    })
}

/// Opens a render pass from its full descriptor. WebGPU's maxDrawCount is
/// a browser's indirect-draw bound; wgpu has no equivalent to pass it to.
pub unsafe fn render_pass_begin_with(encoder: i32, descriptor: &GpuRenderPassDescriptor) {
    let entry = find!(ENCODERS, encoder);
    let PassPlan {
        colors,
        depth,
        writes,
        occlusion,
    } = match render_plan_pass(descriptor) {
        Ok(plan) => plan,
        Err(message) => return raise(&message),
    };
    let label = label(&descriptor.label);
    let mut held = entry.lock().unwrap();
    if held.pass.is_some() || held.compute.is_some() {
        return host::raise(ErrorKind::Runtime, "a pass is already open on this encoder");
    }
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    let attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = colors
        .iter()
        .map(|color| {
            color.as_ref().map(|c| wgpu::RenderPassColorAttachment {
                view: &c.view,
                depth_slice: c.depth_slice,
                resolve_target: c.resolve.as_deref(),
                ops: c.ops,
            })
        })
        .collect();
    let pass = encoder
        .begin_render_pass(&wgpu::RenderPassDescriptor {
            label: label.as_deref(),
            color_attachments: &attachments,
            depth_stencil_attachment: depth.as_ref().map(|d| {
                wgpu::RenderPassDepthStencilAttachment {
                    view: &d.view,
                    depth_ops: d.depth,
                    stencil_ops: d.stencil,
                }
            }),
            timestamp_writes: writes.as_ref().map(|(set, beginning, end)| {
                wgpu::RenderPassTimestampWrites {
                    query_set: &set.set,
                    beginning_of_pass_write_index: *beginning,
                    end_of_pass_write_index: *end,
                }
            }),
            occlusion_query_set: occlusion.as_ref().map(|set| &set.set),
            multiview_mask: None,
        })
        .forget_lifetime();
    held.pass = Some(pass);
}

pub unsafe fn compute_pass_begin_with(encoder: i32, descriptor: &GpuComputePassDescriptor) {
    let entry = find!(ENCODERS, encoder);
    let writes = match descriptor
        .timestampWrites
        .as_ref()
        .map(|w| {
            timestamp_writes(
                w.querySet,
                w.beginningOfPassWriteIndex,
                w.endOfPassWriteIndex,
            )
        })
        .transpose()
    {
        Ok(writes) => writes,
        Err(message) => return raise(&message),
    };
    let label = label(&descriptor.label);
    let mut held = entry.lock().unwrap();
    if held.pass.is_some() || held.compute.is_some() {
        return host::raise(ErrorKind::Runtime, "a pass is already open on this encoder");
    }
    let Some(encoder) = held.encoder.as_mut() else {
        return;
    };
    let pass = encoder
        .begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: label.as_deref(),
            timestamp_writes: writes.as_ref().map(|(set, beginning, end)| {
                wgpu::ComputePassTimestampWrites {
                    query_set: &set.set,
                    beginning_of_pass_write_index: *beginning,
                    end_of_pass_write_index: *end,
                }
            }),
        })
        .forget_lifetime();
    held.compute = Some(pass);
}

// -- commands inside a render pass ----------------------------------------------------------

/// Runs `body` on the encoder's open render pass, or raises.
fn rendering(encoder: &Encoder, body: impl FnOnce(&mut wgpu::RenderPass<'static>)) {
    let mut held = encoder.lock().unwrap();
    match held.pass.as_mut() {
        Some(pass) => body(pass),
        None => host::raise(ErrorKind::Runtime, "no render pass is open on this encoder"),
    }
}

fn count(value: i32, what: &str) -> Option<u32> {
    match index(value, what) {
        Ok(value) => Some(value),
        Err(message) => {
            raise(&message);
            None
        }
    }
}

/// `offset..offset + size`, or to the end of the buffer for a negative size.
pub(super) fn slice(
    buffer: &wgpu::Buffer,
    offset: i64,
    size_of: i64,
) -> Option<wgpu::BufferSlice<'_>> {
    let Ok(start) = size(offset, "offset") else {
        raise("negative buffer offset");
        return None;
    };
    Some(if size_of < 0 {
        buffer.slice(start..)
    } else {
        buffer.slice(start..start.saturating_add(size_of as u64))
    })
}

pub unsafe fn render_draw_range(
    encoder: i32,
    vertex_count: i32,
    instance_count: i32,
    first_vertex: i32,
    first_instance: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let (Some(vertices), Some(instances), Some(first), Some(first_instance)) = (
        count(vertex_count, "vertex count"),
        count(instance_count, "instance count"),
        count(first_vertex, "first vertex"),
        count(first_instance, "first instance"),
    ) else {
        return;
    };
    rendering(&entry, |pass| {
        pass.draw(
            first..first.saturating_add(vertices),
            first_instance..first_instance.saturating_add(instances),
        )
    });
}

pub unsafe fn render_draw_indexed_range(
    encoder: i32,
    index_count: i32,
    instance_count: i32,
    first_index: i32,
    base_vertex: i32,
    first_instance: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let (Some(indices), Some(instances), Some(first), Some(first_instance)) = (
        count(index_count, "index count"),
        count(instance_count, "instance count"),
        count(first_index, "first index"),
        count(first_instance, "first instance"),
    ) else {
        return;
    };
    rendering(&entry, |pass| {
        pass.draw_indexed(
            first..first.saturating_add(indices),
            base_vertex,
            first_instance..first_instance.saturating_add(instances),
        )
    });
}

pub unsafe fn render_set_vertex_buffer_range(
    encoder: i32,
    slot: i32,
    buffer: i32,
    offset: i64,
    size_of: i64,
) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let Some(slot) = count(slot, "vertex buffer slot") else {
        return;
    };
    let Some(range) = slice(&buffer, offset, size_of) else {
        return;
    };
    rendering(&entry, |pass| pass.set_vertex_buffer(slot, range));
}

pub unsafe fn render_set_index_buffer_range(
    encoder: i32,
    buffer: i32,
    format: i32,
    offset: i64,
    size_of: i64,
) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let Some(range) = slice(&buffer, offset, size_of) else {
        return;
    };
    rendering(&entry, |pass| {
        pass.set_index_buffer(range, index_format(format))
    });
}

pub unsafe fn render_multi_draw_indirect(encoder: i32, buffer: i32, offset: i64, count_of: i32) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let Some(draws) = count(count_of, "draw count") else {
        return;
    };
    rendering(&entry, |pass| {
        pass.multi_draw_indirect(&buffer, offset.max(0) as u64, draws)
    });
}

pub unsafe fn render_multi_draw_indexed_indirect(
    encoder: i32,
    buffer: i32,
    offset: i64,
    count_of: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let Some(draws) = count(count_of, "draw count") else {
        return;
    };
    rendering(&entry, |pass| {
        pass.multi_draw_indexed_indirect(&buffer, offset.max(0) as u64, draws)
    });
}

pub unsafe fn render_multi_draw_indirect_count(
    encoder: i32,
    buffer: i32,
    offset: i64,
    count_buffer: i32,
    count_offset: i64,
    max_count: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let count_buffer = find!(BUFFERS, count_buffer);
    let Some(max) = count(max_count, "max draw count") else {
        return;
    };
    rendering(&entry, |pass| {
        pass.multi_draw_indirect_count(
            &buffer,
            offset.max(0) as u64,
            &count_buffer,
            count_offset.max(0) as u64,
            max,
        )
    });
}

pub unsafe fn render_multi_draw_indexed_indirect_count(
    encoder: i32,
    buffer: i32,
    offset: i64,
    count_buffer: i32,
    count_offset: i64,
    max_count: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    let count_buffer = find!(BUFFERS, count_buffer);
    let Some(max) = count(max_count, "max draw count") else {
        return;
    };
    rendering(&entry, |pass| {
        pass.multi_draw_indexed_indirect_count(
            &buffer,
            offset.max(0) as u64,
            &count_buffer,
            count_offset.max(0) as u64,
            max,
        )
    });
}

/// `size` bytes from byte `start` of a shared buffer, checked first.
fn immediate_bytes(data: &Buffer, start: i64, size_of: i32) -> Option<&[u8]> {
    let (Ok(start), Ok(len)) = (usize::try_from(start), usize::try_from(size_of)) else {
        raise("negative immediate data range");
        return None;
    };
    if start.checked_add(len).is_none_or(|end| end > data.len()) {
        raise("immediate data exceeds the shared buffer");
        return None;
    }
    Some(unsafe { &data.as_slice()[start..start + len] })
}

pub unsafe fn render_set_immediates(
    encoder: i32,
    offset: i32,
    data: Buffer,
    start: i64,
    size_of: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let (Some(offset), Some(bytes)) = (
        count(offset, "immediate offset"),
        immediate_bytes(&data, start, size_of),
    ) else {
        return;
    };
    rendering(&entry, |pass| pass.set_immediates(offset, bytes));
}

pub unsafe fn compute_set_immediates(
    encoder: i32,
    offset: i32,
    data: Buffer,
    start: i64,
    size_of: i32,
) {
    let entry = find!(ENCODERS, encoder);
    let (Some(offset), Some(bytes)) = (
        count(offset, "immediate offset"),
        immediate_bytes(&data, start, size_of),
    ) else {
        return;
    };
    computing(&entry, |pass| pass.set_immediates(offset, bytes));
}

pub unsafe fn render_begin_occlusion_query(encoder: i32, index_of: i32) {
    let entry = find!(ENCODERS, encoder);
    let Some(query) = count(index_of, "query index") else {
        return;
    };
    rendering(&entry, |pass| pass.begin_occlusion_query(query));
}

pub unsafe fn render_end_occlusion_query(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    rendering(&entry, |pass| pass.end_occlusion_query());
}

pub unsafe fn render_begin_pipeline_statistics(encoder: i32, set: i32, index_of: i32) {
    let entry = find!(ENCODERS, encoder);
    let set = find!(QUERY_SETS, set);
    let Some(query) = count(index_of, "query index") else {
        return;
    };
    rendering(&entry, |pass| {
        pass.begin_pipeline_statistics_query(&set.set, query)
    });
}

pub unsafe fn render_end_pipeline_statistics(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    rendering(&entry, |pass| pass.end_pipeline_statistics_query());
}

pub unsafe fn compute_begin_pipeline_statistics(encoder: i32, set: i32, index_of: i32) {
    let entry = find!(ENCODERS, encoder);
    let set = find!(QUERY_SETS, set);
    let Some(query) = count(index_of, "query index") else {
        return;
    };
    computing(&entry, |pass| {
        pass.begin_pipeline_statistics_query(&set.set, query)
    });
}

pub unsafe fn compute_end_pipeline_statistics(encoder: i32) {
    let entry = find!(ENCODERS, encoder);
    computing(&entry, |pass| pass.end_pipeline_statistics_query());
}

pub unsafe fn render_write_timestamp(encoder: i32, set: i32, index_of: i32) {
    let entry = find!(ENCODERS, encoder);
    let set = find!(QUERY_SETS, set);
    let Some(query) = count(index_of, "query index") else {
        return;
    };
    rendering(&entry, |pass| pass.write_timestamp(&set.set, query));
}

pub unsafe fn compute_write_timestamp(encoder: i32, set: i32, index_of: i32) {
    let entry = find!(ENCODERS, encoder);
    let set = find!(QUERY_SETS, set);
    let Some(query) = count(index_of, "query index") else {
        return;
    };
    computing(&entry, |pass| pass.write_timestamp(&set.set, query));
}

pub unsafe fn render_execute_bundle(encoder: i32, bundle: i32) {
    let entry = find!(ENCODERS, encoder);
    let bundle = find!(BUNDLES, bundle);
    rendering(&entry, |pass| {
        pass.execute_bundles(std::iter::once(&*bundle))
    });
}
