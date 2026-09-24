//! Mesh pipelines and mesh draws, wgpu's own with EXPERIMENTAL_MESH_SHADER.

use std::num::NonZeroU32;
use std::sync::Arc;

use super::render::{
    Stage, color_target, count, depth_stencil, label, multisample, options, pipeline_layout,
    primitive, rendering, settle_pipeline,
};
use super::*;
use crate::GpuMeshPipelineDescriptor;

struct MeshPlan {
    device: wgpu::Device,
    label: Option<String>,
    layout: Option<Arc<wgpu::PipelineLayout>>,
    task: Option<Stage>,
    mesh: Stage,
    primitive: wgpu::PrimitiveState,
    depth_stencil: Option<wgpu::DepthStencilState>,
    multisample: wgpu::MultisampleState,
    fragment: Option<(Stage, Vec<Option<wgpu::ColorTargetState>>)>,
    multiview: Option<NonZeroU32>,
}

fn mesh_plan(device: i32, d: &GpuMeshPipelineDescriptor) -> Result<MeshPlan, String> {
    let entry = DEVICES
        .lock()
        .unwrap()
        .get(device)
        .ok_or("the device was destroyed")?;
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
    let multiview = match d.multiview {
        None => None,
        Some(layers) => {
            Some(NonZeroU32::new(index(layers, "multiview")?).ok_or("a multiview of 0 layers")?)
        }
    };
    Ok(MeshPlan {
        device: entry.device.clone(),
        label: label(&d.label),
        layout: pipeline_layout(d.layout)?,
        task: match &d.task {
            None => None,
            Some(t) => Some(Stage::of(t.module, &t.entryPoint, &t.constants)?),
        },
        mesh: Stage::of(d.mesh.module, &d.mesh.entryPoint, &d.mesh.constants)?,
        primitive: primitive(d.primitive.as_ref()),
        depth_stencil: d.depthStencil.as_ref().map(depth_stencil),
        multisample: multisample(d.multisample.as_ref())?,
        fragment,
        multiview,
    })
}

fn build_mesh(plan: &MeshPlan) -> wgpu::RenderPipeline {
    let task_constants = plan.task.as_ref().map(Stage::pairs).unwrap_or_default();
    let mesh_constants = plan.mesh.pairs();
    let fragment_constants = plan
        .fragment
        .as_ref()
        .map(|(stage, _)| stage.pairs())
        .unwrap_or_default();
    plan.device
        .create_mesh_pipeline(&wgpu::MeshPipelineDescriptor {
            label: plan.label.as_deref(),
            layout: plan.layout.as_deref(),
            task: plan.task.as_ref().map(|stage| wgpu::TaskState {
                module: &stage.module,
                entry_point: stage.entry.as_deref(),
                compilation_options: options(&task_constants),
            }),
            mesh: wgpu::MeshState {
                module: &plan.mesh.module,
                entry_point: plan.mesh.entry.as_deref(),
                compilation_options: options(&mesh_constants),
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
            multiview: plan.multiview,
            cache: None,
        })
}

pub unsafe fn mesh_pipeline_create(device: i32, descriptor: &GpuMeshPipelineDescriptor) -> i32 {
    match mesh_plan(device, descriptor) {
        Ok(plan) => RENDER_PIPELINES.lock().unwrap().put(build_mesh(&plan)),
        Err(message) => refuse(&message),
    }
}

pub unsafe fn mesh_pipeline_create_async(
    device: i32,
    descriptor: &GpuMeshPipelineDescriptor,
) -> Future<crate::GpuPipeline> {
    let plan = match mesh_plan(device, descriptor) {
        Ok(plan) => plan,
        Err(message) => return rejected_future(&message),
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    spawn_gpu(async move {
        let handle = RENDER_PIPELINES.lock().unwrap().put(build_mesh(&plan));
        settle_pipeline(&completion, handle);
    });
    future
}

pub unsafe fn render_draw_mesh_tasks(encoder: i32, x: i32, y: i32, z: i32) {
    let entry = find!(ENCODERS, encoder);
    let (Some(x), Some(y), Some(z)) = (
        count(x, "group count x"),
        count(y, "group count y"),
        count(z, "group count z"),
    ) else {
        return;
    };
    rendering(&entry, |pass| pass.draw_mesh_tasks(x, y, z));
}

pub unsafe fn render_draw_mesh_tasks_indirect(encoder: i32, buffer: i32, offset: i64) {
    let entry = find!(ENCODERS, encoder);
    let buffer = find!(BUFFERS, buffer);
    rendering(&entry, |pass| {
        pass.draw_mesh_tasks_indirect(&buffer, offset.max(0) as u64)
    });
}

pub unsafe fn render_multi_draw_mesh_tasks_indirect(
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
        pass.multi_draw_mesh_tasks_indirect(&buffer, offset.max(0) as u64, draws)
    });
}

pub unsafe fn render_multi_draw_mesh_tasks_indirect_count(
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
        pass.multi_draw_mesh_tasks_indirect_count(
            &buffer,
            offset.max(0) as u64,
            &count_buffer,
            count_offset.max(0) as u64,
            max,
        )
    });
}
