//! wgpu's own features and limits beyond WebGPU's, and what an adapter
//! supports for each texture format.

use super::{ADAPTERS, DEVICES, texture_format};

/// A `NativeFeature` code: its position in `wgpu::Features::all()`, the
/// order build.rs generated the enum in.
fn native_feature(which: i32) -> Option<wgpu::Features> {
    let index = usize::try_from(which).ok()?;
    wgpu::Features::all()
        .iter_names()
        .nth(index)
        .map(|(_, flag)| flag)
}

pub(super) fn requested_features(requested: &[i32]) -> Result<wgpu::Features, String> {
    requested
        .iter()
        .try_fold(wgpu::Features::empty(), |all, &which| {
            native_feature(which)
                .map(|flag| all | flag)
                .ok_or_else(|| format!("unknown native feature {which}"))
        })
}

pub unsafe fn adapter_native_feature(adapter: i32, which: i32) -> bool {
    let adapter = find!(ADAPTERS, adapter, false);
    native_feature(which).is_some_and(|flag| adapter.features().contains(flag))
}

pub unsafe fn device_native_feature(device: i32, which: i32) -> bool {
    let device = find!(DEVICES, device, false);
    native_feature(which).is_some_and(|flag| device.device.features().contains(flag))
}

/// A `NativeLimit` code's field, in the declaration's order.
fn native_limit(limits: &mut wgpu::Limits, which: i32) -> Option<&mut u32> {
    Some(match which {
        0 => &mut limits.max_binding_array_elements_per_shader_stage,
        1 => &mut limits.max_binding_array_acceleration_structure_elements_per_shader_stage,
        2 => &mut limits.max_binding_array_sampler_elements_per_shader_stage,
        3 => &mut limits.max_non_sampler_bindings,
        4 => &mut limits.max_task_workgroup_total_count,
        5 => &mut limits.max_task_workgroups_per_dimension,
        6 => &mut limits.max_mesh_workgroup_total_count,
        7 => &mut limits.max_mesh_workgroups_per_dimension,
        8 => &mut limits.max_task_invocations_per_workgroup,
        9 => &mut limits.max_task_invocations_per_dimension,
        10 => &mut limits.max_mesh_invocations_per_workgroup,
        11 => &mut limits.max_mesh_invocations_per_dimension,
        12 => &mut limits.max_task_payload_size,
        13 => &mut limits.max_mesh_output_vertices,
        14 => &mut limits.max_mesh_output_primitives,
        15 => &mut limits.max_mesh_output_layers,
        16 => &mut limits.max_mesh_multiview_view_count,
        17 => &mut limits.max_blas_primitive_count,
        18 => &mut limits.max_blas_geometry_count,
        19 => &mut limits.max_tlas_instance_count,
        20 => &mut limits.max_acceleration_structures_per_shader_stage,
        21 => &mut limits.max_buffers_and_acceleration_structures_per_shader_stage,
        22 => &mut limits.max_multiview_view_count,
        23 => &mut limits.max_ray_dispatch_count,
        24 => &mut limits.max_ray_recursion_depth,
        _ => return None,
    })
}

pub(super) fn requested_limits(
    mut limits: wgpu::Limits,
    requested: &[(i32, i64)],
) -> Result<wgpu::Limits, String> {
    for &(which, value) in requested {
        let value = u32::try_from(value)
            .map_err(|_| format!("native limit value {value} is outside u32"))?;
        *native_limit(&mut limits, which)
            .ok_or_else(|| format!("unknown native limit {which}"))? = value;
    }
    Ok(limits)
}

fn limit_of(mut limits: wgpu::Limits, which: i32) -> i64 {
    native_limit(&mut limits, which).map_or(-1, |value| i64::from(*value))
}

pub unsafe fn adapter_native_limit(adapter: i32, which: i32) -> i64 {
    let adapter = find!(ADAPTERS, adapter, -1);
    limit_of(adapter.limits(), which)
}

pub unsafe fn device_native_limit(device: i32, which: i32) -> i64 {
    let device = find!(DEVICES, device, -1);
    limit_of(device.device.limits(), which)
}

pub unsafe fn adapter_format_usages(adapter: i32, format: i32) -> i32 {
    let adapter = find!(ADAPTERS, adapter, 0);
    adapter
        .get_texture_format_features(texture_format(format))
        .allowed_usages
        .bits() as i32
}

/// TextureFormatFeature bits, which are wgpu's TextureFormatFeatureFlags.
pub unsafe fn adapter_format_features(adapter: i32, format: i32) -> i32 {
    let adapter = find!(ADAPTERS, adapter, 0);
    adapter
        .get_texture_format_features(texture_format(format))
        .flags
        .bits() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_codes_follow_wgpus_catalog() {
        assert_eq!(native_feature(0), wgpu::Features::all().iter().next());
        let count = wgpu::Features::all().iter_names().count() as i32;
        assert!(native_feature(count - 1).is_some());
        assert!(native_feature(count).is_none());
        assert!(requested_features(&[count]).is_err());
        let mut limits = wgpu::Limits::default();
        assert!(native_limit(&mut limits, 24).is_some());
        assert!(native_limit(&mut limits, 25).is_none());
        let raised = requested_limits(wgpu::Limits::default(), &[(19, 64)]).unwrap();
        assert_eq!(raised.max_tlas_instance_count, 64);
        assert!(requested_limits(wgpu::Limits::default(), &[(19, -1)]).is_err());
        // The declaration's feature flags match wgpu's.
        assert_eq!(
            crate::TextureFormatFeature::BLENDABLE() as u32,
            wgpu::TextureFormatFeatureFlags::BLENDABLE.bits()
        );
        assert_eq!(
            crate::PipelineStatistic::COMPUTE_SHADER_INVOCATIONS() as u8,
            wgpu::PipelineStatisticsTypes::COMPUTE_SHADER_INVOCATIONS.bits()
        );
    }
}
