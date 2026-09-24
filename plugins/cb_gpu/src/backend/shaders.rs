//! Shaders wgpu does not check: WGSL with runtime checks turned off, and
//! backend code handed over as it is. Both need `trustedShaders` on the
//! device descriptor.

use std::borrow::Cow;

use super::*;
use crate::{GpuPassthroughShaderDescriptor, GpuShaderModuleDescriptor};

const UNTRUSTED: &str = "needs trustedShaders(true) on the device descriptor";

pub unsafe fn shader_create_with(device: i32, d: &GpuShaderModuleDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let on = |check: Option<bool>| check.unwrap_or(true);
    // Each field is set as the program asked; `checked()` is all of them on.
    let mut checks = wgpu::ShaderRuntimeChecks::checked();
    checks.bounds_checks = on(d.boundsChecks);
    checks.force_loop_bounding = on(d.forceLoopBounding);
    checks.ray_query_initialization_tracking = on(d.rayQueryInitializationTracking);
    checks.task_shader_dispatch_tracking = on(d.taskShaderDispatchTracking);
    checks.mesh_shader_primitive_indices_clamp = on(d.meshShaderPrimitiveIndicesClamp);
    checks.int_div_checks = on(d.intDivChecks);
    let all_on = [
        checks.bounds_checks,
        checks.force_loop_bounding,
        checks.ray_query_initialization_tracking,
        checks.task_shader_dispatch_tracking,
        checks.mesh_shader_primitive_indices_clamp,
        checks.int_div_checks,
    ]
    .iter()
    .all(|&on| on);
    if !all_on && !entry.trusted_shaders {
        return refuse(&format!("turning off a runtime check {UNTRUSTED}"));
    }
    let code = d.code.get();
    let label = d.label.as_ref().map(caribou_abi::Rooted::get);
    let descriptor = wgpu::ShaderModuleDescriptor {
        label: label.as_ref().map(Text::as_str),
        source: wgpu::ShaderSource::Wgsl(code.as_str().into()),
    };
    let module = if all_on {
        entry.device.create_shader_module(descriptor)
    } else {
        // The device's trustedShaders: the program vouches that the shader
        // needs none of the checks it turned off.
        unsafe {
            entry
                .device
                .create_shader_module_trusted(descriptor, checks)
        }
    };
    SHADERS.lock().unwrap().put(module)
}

fn words(bytes: &[u8]) -> Result<Vec<u32>, String> {
    if !bytes.len().is_multiple_of(4) {
        return Err("SPIR-V is a whole number of 4-byte words".into());
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&w| u32::from_le_bytes(w))
        .collect())
}

pub unsafe fn shader_create_passthrough(device: i32, d: &GpuPassthroughShaderDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    if !entry.trusted_shaders {
        return refuse(&format!("a passthrough shader {UNTRUSTED}"));
    }
    let bytes = |b: &Option<caribou_abi::Rooted<Buffer>>| {
        b.as_ref().map(|b| {
            let buffer = b.get();
            unsafe { buffer.as_slice() }.to_vec()
        })
    };
    let text =
        |t: &Option<caribou_abi::Rooted<Text>>| t.as_ref().map(|t| t.get().as_str().to_owned());
    let spirv = match bytes(&d.spirv).map(|b| words(&b)).transpose() {
        Ok(spirv) => spirv,
        Err(message) => return refuse(&message),
    };
    let dxil = bytes(&d.dxil);
    let metallib = bytes(&d.metallib);
    let (hlsl, msl, wgsl) = (text(&d.hlsl), text(&d.msl), text(&d.wgsl));
    // wgpu panics when the backend in use has no source of its own.
    let has_source = match entry.device.adapter_info().backend {
        wgpu::Backend::Vulkan => spirv.is_some(),
        wgpu::Backend::Dx12 => dxil.is_some() || hlsl.is_some(),
        wgpu::Backend::Metal => metallib.is_some() || msl.is_some(),
        wgpu::Backend::BrowserWebGpu => wgsl.is_some(),
        _ => false,
    };
    if !has_source {
        return refuse("the passthrough shader has no source for this device's backend");
    }
    let entry_points = match d
        .entryPoints
        .iter()
        .map(|e| {
            Ok(wgpu::PassthroughShaderEntryPoint {
                name: Cow::Owned(e.name.get().as_str().to_owned()),
                workgroup_size: (
                    index(e.workgroupX.unwrap_or(1), "workgroupX")?,
                    index(e.workgroupY.unwrap_or(1), "workgroupY")?,
                    index(e.workgroupZ.unwrap_or(1), "workgroupZ")?,
                ),
            })
        })
        .collect::<Result<Vec<_>, String>>()
    {
        Ok(entry_points) => entry_points,
        Err(message) => return refuse(&message),
    };
    let label = d.label.as_ref().map(caribou_abi::Rooted::get);
    let descriptor = wgpu::ShaderModuleDescriptorPassthrough {
        label: label.as_ref().map(Text::as_str),
        entry_points: Cow::Owned(entry_points),
        spirv: spirv.map(Cow::Owned),
        dxil: dxil.map(Cow::Owned),
        hlsl: hlsl.map(Cow::Owned),
        metallib: metallib.map(Cow::Owned),
        msl: msl.map(Cow::Owned),
        glsl: None,
        wgsl: wgsl.map(Cow::Owned),
    };
    // The device's trustedShaders: the program vouches for this code, which
    // goes to the backend unvalidated.
    let module = unsafe { entry.device.create_shader_module_passthrough(descriptor) };
    SHADERS.lock().unwrap().put(module)
}
