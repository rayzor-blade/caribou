//! What adapters, buffers and textures report about themselves, and writes
//! into mapped buffers.

use super::*;

pub unsafe fn adapter_vendor_id(adapter: i32) -> i64 {
    i64::from(find!(ADAPTERS, adapter, 0).get_info().vendor)
}

pub unsafe fn adapter_device_id(adapter: i32) -> i64 {
    i64::from(find!(ADAPTERS, adapter, 0).get_info().device)
}

pub unsafe fn adapter_device_type(adapter: i32) -> i32 {
    match find!(ADAPTERS, adapter, 0).get_info().device_type {
        wgpu::DeviceType::Other => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::DiscreteGpu => 2,
        wgpu::DeviceType::VirtualGpu => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

pub unsafe fn adapter_pci_bus_id(adapter: i32) -> Text {
    let adapter = find!(ADAPTERS, adapter, Text::NULL);
    Text::new(&adapter.get_info().device_pci_bus_id)
}

pub unsafe fn adapter_subgroup_min_size(adapter: i32) -> i32 {
    find!(ADAPTERS, adapter, 0).get_info().subgroup_min_size as i32
}

pub unsafe fn adapter_subgroup_max_size(adapter: i32) -> i32 {
    find!(ADAPTERS, adapter, 0).get_info().subgroup_max_size as i32
}

pub unsafe fn buffer_size(buffer: i32) -> i64 {
    find!(BUFFERS, buffer, 0).size() as i64
}

pub unsafe fn buffer_usage(buffer: i32) -> i32 {
    find!(BUFFERS, buffer, 0).usage().bits() as i32
}

/// The first `len` bytes of `data` into the mapped range at `offset`. False
/// when that range is not mapped for writing.
pub unsafe fn buffer_copy_in(buffer: i32, offset: i64, data: Buffer, len: i32) -> bool {
    let Some(source) = bytes(&data, len) else {
        return false;
    };
    let buffer = find!(BUFFERS, buffer, false);
    let start = offset.max(0) as u64;
    let Ok(mut view) = buffer.get_mapped_range_mut(start..start + source.len() as u64) else {
        return false;
    };
    view.copy_from_slice(source);
    true
}

pub unsafe fn texture_width(texture: i32) -> i32 {
    find!(TEXTURES, texture, 0).width() as i32
}

pub unsafe fn texture_height(texture: i32) -> i32 {
    find!(TEXTURES, texture, 0).height() as i32
}

pub unsafe fn texture_depth_or_array_layers(texture: i32) -> i32 {
    find!(TEXTURES, texture, 0).depth_or_array_layers() as i32
}

pub unsafe fn texture_mip_level_count(texture: i32) -> i32 {
    find!(TEXTURES, texture, 0).mip_level_count() as i32
}

pub unsafe fn texture_sample_count(texture: i32) -> i32 {
    find!(TEXTURES, texture, 0).sample_count() as i32
}

pub unsafe fn texture_get_dimension(texture: i32) -> i32 {
    match find!(TEXTURES, texture, 1).dimension() {
        wgpu::TextureDimension::D1 => 0,
        wgpu::TextureDimension::D2 => 1,
        wgpu::TextureDimension::D3 => 2,
    }
}

/// Every texture made here has a format the declaration names.
pub unsafe fn texture_get_format(texture: i32) -> i32 {
    let format = find!(TEXTURES, texture, 0).format();
    super::texture_format_code(format).unwrap_or_else(|| {
        host::raise(ErrorKind::Runtime, "the texture's format has no name here");
        0
    })
}

pub unsafe fn texture_usage(texture: i32) -> i32 {
    find!(TEXTURES, texture, 0).usage().bits() as i32
}
