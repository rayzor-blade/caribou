//! Copies with their full source and destination descriptions: mip level,
//! origin, aspect, buffer layout and extent.

use super::*;
use crate::{
    GpuExtent3D, GpuTexelCopyBufferInfo, GpuTexelCopyBufferLayout, GpuTexelCopyTextureInfo,
};

fn positive(value: i32, what: &str) -> Result<u32, String> {
    index(value, what)
}

fn texture_info<'a>(
    info: &GpuTexelCopyTextureInfo,
    texture: &'a wgpu::Texture,
) -> Result<wgpu::TexelCopyTextureInfo<'a>, String> {
    let origin = match &info.origin {
        None => wgpu::Origin3d::ZERO,
        Some(o) => wgpu::Origin3d {
            x: positive(o.x.unwrap_or(0), "origin x")?,
            y: positive(o.y.unwrap_or(0), "origin y")?,
            z: positive(o.z.unwrap_or(0), "origin z")?,
        },
    };
    Ok(wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: positive(info.mipLevel.unwrap_or(0), "mip level")?,
        origin,
        aspect: texture_aspect(info.aspect.unwrap_or(0)),
    })
}

fn buffer_layout(
    offset: Option<i64>,
    bytes_per_row: Option<i32>,
    rows_per_image: Option<i32>,
) -> Result<wgpu::TexelCopyBufferLayout, String> {
    Ok(wgpu::TexelCopyBufferLayout {
        offset: size(offset.unwrap_or(0), "offset")?,
        bytes_per_row: bytes_per_row
            .map(|b| positive(b, "bytes per row"))
            .transpose()?,
        rows_per_image: rows_per_image
            .map(|r| positive(r, "rows per image"))
            .transpose()?,
    })
}

fn extent_of(e: &GpuExtent3D) -> Result<wgpu::Extent3d, String> {
    Ok(wgpu::Extent3d {
        width: positive(e.width, "width")?,
        height: positive(e.height.unwrap_or(1), "height")?,
        depth_or_array_layers: positive(
            e.depthOrArrayLayers.unwrap_or(1),
            "depth or array layers",
        )?,
    })
}

fn texture_of(handle: i32) -> Result<Arc<wgpu::Texture>, String> {
    TEXTURES
        .lock()
        .unwrap()
        .get(handle)
        .ok_or_else(|| "the texture was destroyed".into())
}

fn buffer_of(handle: i32) -> Result<Arc<wgpu::Buffer>, String> {
    BUFFERS
        .lock()
        .unwrap()
        .get(handle)
        .ok_or_else(|| "the buffer was destroyed".into())
}

/// Runs `body` on the encoder outside any pass, raising what it refuses.
fn copying(handle: i32, body: impl FnOnce(&mut wgpu::CommandEncoder) -> Result<(), String>) {
    let Some(entry) = ENCODERS.lock().unwrap().get(handle) else {
        return;
    };
    let mut held = entry.lock().unwrap();
    if held.pass.is_some() || held.compute.is_some() {
        return host::raise(ErrorKind::Runtime, "a pass is open on this encoder");
    }
    if let Some(encoder) = held.encoder.as_mut()
        && let Err(message) = body(encoder)
    {
        host::raise(ErrorKind::Type, &message);
    }
}

pub unsafe fn encoder_copy_buffer_to_texture_with(
    encoder: i32,
    source: &GpuTexelCopyBufferInfo,
    destination: &GpuTexelCopyTextureInfo,
    extent: &GpuExtent3D,
) {
    copying(encoder, |encoder| {
        let buffer = buffer_of(source.buffer)?;
        let texture = texture_of(destination.texture)?;
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: buffer_layout(source.offset, source.bytesPerRow, source.rowsPerImage)?,
            },
            texture_info(destination, &texture)?,
            extent_of(extent)?,
        );
        Ok(())
    });
}

pub unsafe fn encoder_copy_texture_to_buffer_with(
    encoder: i32,
    source: &GpuTexelCopyTextureInfo,
    destination: &GpuTexelCopyBufferInfo,
    extent: &GpuExtent3D,
) {
    copying(encoder, |encoder| {
        let texture = texture_of(source.texture)?;
        let buffer = buffer_of(destination.buffer)?;
        encoder.copy_texture_to_buffer(
            texture_info(source, &texture)?,
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: buffer_layout(
                    destination.offset,
                    destination.bytesPerRow,
                    destination.rowsPerImage,
                )?,
            },
            extent_of(extent)?,
        );
        Ok(())
    });
}

pub unsafe fn encoder_copy_texture_to_texture_with(
    encoder: i32,
    source: &GpuTexelCopyTextureInfo,
    destination: &GpuTexelCopyTextureInfo,
    extent: &GpuExtent3D,
) {
    copying(encoder, |encoder| {
        let from = texture_of(source.texture)?;
        let to = texture_of(destination.texture)?;
        encoder.copy_texture_to_texture(
            texture_info(source, &from)?,
            texture_info(destination, &to)?,
            extent_of(extent)?,
        );
        Ok(())
    });
}

/// Every mip level and layer to zero, with the CLEAR_TEXTURE feature.
pub unsafe fn encoder_clear_texture(encoder: i32, texture: i32) {
    copying(encoder, |encoder| {
        let texture = texture_of(texture)?;
        encoder.clear_texture(&texture, &wgpu::ImageSubresourceRange::default());
        Ok(())
    });
}

/// Uploads the whole shared buffer through the given layout.
pub unsafe fn queue_write_texture_with(
    queue: i32,
    destination: &GpuTexelCopyTextureInfo,
    data: Buffer,
    layout: &GpuTexelCopyBufferLayout,
    extent: &GpuExtent3D,
) {
    let queue = find!(QUEUES, queue);
    let written = (|| {
        let texture = texture_of(destination.texture)?;
        queue.write_texture(
            texture_info(destination, &texture)?,
            unsafe { data.as_slice() },
            buffer_layout(layout.offset, layout.bytesPerRow, layout.rowsPerImage)?,
            extent_of(extent)?,
        );
        Ok::<_, String>(())
    })();
    if let Err(message) = written {
        host::raise(ErrorKind::Type, &message);
    }
}
