//! Surface configuration beyond format and size, and what a surface
//! supports on an adapter.

use super::*;
use crate::GpuSurfaceConfiguration;

pub struct Capabilities {
    formats: Vec<i32>,
    present_modes: Vec<i32>,
    alpha_modes: Vec<i32>,
    usages: u32,
}

// The declaration's orders are wgpu's own for these three.
fn present_mode(value: i32) -> wgpu::PresentMode {
    match value {
        0 => wgpu::PresentMode::AutoVsync,
        1 => wgpu::PresentMode::AutoNoVsync,
        3 => wgpu::PresentMode::FifoRelaxed,
        4 => wgpu::PresentMode::Immediate,
        5 => wgpu::PresentMode::Mailbox,
        _ => wgpu::PresentMode::Fifo,
    }
}

fn alpha_mode(value: i32) -> wgpu::CompositeAlphaMode {
    match value {
        1 => wgpu::CompositeAlphaMode::Opaque,
        2 => wgpu::CompositeAlphaMode::PreMultiplied,
        3 => wgpu::CompositeAlphaMode::PostMultiplied,
        4 => wgpu::CompositeAlphaMode::Inherit,
        _ => wgpu::CompositeAlphaMode::Auto,
    }
}

fn color_space(value: i32) -> wgpu::SurfaceColorSpace {
    match value {
        1 => wgpu::SurfaceColorSpace::Srgb,
        2 => wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
        3 => wgpu::SurfaceColorSpace::DisplayP3,
        _ => wgpu::SurfaceColorSpace::Auto,
    }
}

pub unsafe fn surface_configure_with(device: i32, surface: i32, c: &GpuSurfaceConfiguration) {
    let entry = find!(DEVICES, device);
    let surface = find!(SURFACES, surface);
    let (Ok(width), Ok(height)) = (index(c.width, "width"), index(c.height, "height")) else {
        return host::raise(ErrorKind::Type, "negative surface size");
    };
    let latency = match index(c.desiredMaximumFrameLatency.unwrap_or(2), "frame latency") {
        Ok(latency) => latency,
        Err(message) => return host::raise(ErrorKind::Type, &message),
    };
    let held = surface.lock().unwrap();
    held.surface.configure(
        &entry.device,
        &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::from_bits_truncate(
                c.usage
                    .unwrap_or(wgpu::TextureUsages::RENDER_ATTACHMENT.bits() as i32)
                    as u32,
            ),
            format: texture_format(c.format),
            width: width.max(1),
            height: height.max(1),
            color_space: color_space(c.colorSpace.unwrap_or(0)),
            present_mode: present_mode(c.presentMode.unwrap_or(2)),
            desired_maximum_frame_latency: latency,
            alpha_mode: alpha_mode(c.alphaMode.unwrap_or(0)),
            view_formats: c.viewFormats.iter().copied().map(texture_format).collect(),
        },
    );
}

pub unsafe fn surface_capabilities(surface: i32, adapter: i32) -> i32 {
    let surface = find!(SURFACES, surface, 0);
    let adapter = find!(ADAPTERS, adapter, 0);
    let found = surface.lock().unwrap().surface.get_capabilities(&adapter);
    CAPABILITIES.lock().unwrap().put(Capabilities {
        // A format wgpu has and this plugin does not name is left out.
        formats: found
            .formats
            .into_iter()
            .filter_map(texture_format_code)
            .collect(),
        present_modes: found.present_modes.into_iter().map(|m| m as i32).collect(),
        alpha_modes: found.alpha_modes.into_iter().map(|m| m as i32).collect(),
        usages: found.usages.bits(),
    })
}

pub unsafe fn capabilities_destroy(capabilities: i32) {
    CAPABILITIES.lock().unwrap().remove(capabilities);
}

fn nth(values: &[i32], index_of: i32) -> i32 {
    match usize::try_from(index_of).ok().and_then(|i| values.get(i)) {
        Some(value) => *value,
        None => {
            host::raise(ErrorKind::Type, "capability index out of range");
            0
        }
    }
}

pub unsafe fn capabilities_format_count(c: i32) -> i32 {
    find!(CAPABILITIES, c, 0).formats.len() as i32
}

pub unsafe fn capabilities_format(c: i32, index_of: i32) -> i32 {
    nth(&find!(CAPABILITIES, c, 0).formats, index_of)
}

pub unsafe fn capabilities_present_mode_count(c: i32) -> i32 {
    find!(CAPABILITIES, c, 0).present_modes.len() as i32
}

pub unsafe fn capabilities_present_mode(c: i32, index_of: i32) -> i32 {
    nth(&find!(CAPABILITIES, c, 0).present_modes, index_of)
}

pub unsafe fn capabilities_alpha_mode_count(c: i32) -> i32 {
    find!(CAPABILITIES, c, 0).alpha_modes.len() as i32
}

pub unsafe fn capabilities_alpha_mode(c: i32, index_of: i32) -> i32 {
    nth(&find!(CAPABILITIES, c, 0).alpha_modes, index_of)
}

pub unsafe fn capabilities_usages(c: i32) -> i32 {
    find!(CAPABILITIES, c, 0).usages as i32
}
