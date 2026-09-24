//! External textures, wgpu's own with EXTERNAL_TEXTURE: planes of YUV or
//! RGBA video that a shader samples as one RGBA texture.

use super::*;
use crate::{GpuExternalTextureDescriptor, GpuExternalTextureTransferFunction};

/// `N` values, or the identity of that shape when none were given.
fn matrix<const N: usize>(
    given: &[f32],
    identity: [f32; N],
    what: &str,
) -> Result<[f32; N], String> {
    if given.is_empty() {
        return Ok(identity);
    }
    given
        .try_into()
        .map_err(|_| format!("{what} takes {N} values, not {}", given.len()))
}

fn transfer(
    function: Option<&GpuExternalTextureTransferFunction>,
) -> wgpu::ExternalTextureTransferFunction {
    function.map_or_else(Default::default, |f| {
        wgpu::ExternalTextureTransferFunction {
            a: f.a,
            b: f.b,
            g: f.g,
            k: f.k,
        }
    })
}

const IDENTITY_4X4: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];
const IDENTITY_3X3: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
const IDENTITY_3X2: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

pub unsafe fn external_texture_create(device: i32, d: &GpuExternalTextureDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let described = (|| {
        let format = match d.format {
            0 => wgpu::ExternalTextureFormat::Rgba,
            1 => wgpu::ExternalTextureFormat::Nv12,
            _ => wgpu::ExternalTextureFormat::Yu12,
        };
        let planes = d
            .planes
            .iter()
            .map(|&handle| {
                VIEWS
                    .lock()
                    .unwrap()
                    .get(handle)
                    .ok_or_else(|| "a plane's texture view was destroyed".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let descriptor = wgpu::wgt::ExternalTextureDescriptor {
            label: d.label.as_ref().map(|text| text.get().as_str().to_owned()),
            width: index(d.width.unwrap_or(0), "width")?,
            height: index(d.height.unwrap_or(0), "height")?,
            format,
            yuv_conversion_matrix: matrix(
                &d.yuvConversionMatrix,
                IDENTITY_4X4,
                "yuvConversionMatrix",
            )?,
            gamut_conversion_matrix: matrix(
                &d.gamutConversionMatrix,
                IDENTITY_3X3,
                "gamutConversionMatrix",
            )?,
            src_transfer_function: transfer(d.srcTransferFunction.as_ref()),
            dst_transfer_function: transfer(d.dstTransferFunction.as_ref()),
            sample_transform: matrix(&d.sampleTransform, IDENTITY_3X2, "sampleTransform")?,
            load_transform: matrix(&d.loadTransform, IDENTITY_3X2, "loadTransform")?,
        };
        if planes.len() != descriptor.num_planes() {
            return Err(format!(
                "this format takes {} planes, not {}",
                descriptor.num_planes(),
                planes.len()
            ));
        }
        Ok((descriptor, planes))
    })();
    let (descriptor, planes) = match described {
        Ok(described) => described,
        Err(message) => return refuse(&message),
    };
    let borrowed: Vec<&wgpu::TextureView> = planes.iter().map(|view| &**view).collect();
    let texture = entry
        .device
        .create_external_texture(&descriptor.map_label(|label| label.as_deref()), &borrowed);
    EXTERNAL_TEXTURES.lock().unwrap().put(texture)
}

pub unsafe fn external_texture_destroy(texture: i32) {
    EXTERNAL_TEXTURES.lock().unwrap().remove(texture);
}
