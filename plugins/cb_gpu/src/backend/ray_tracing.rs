//! Acceleration structures for ray queries, wgpu's own with
//! EXPERIMENTAL_RAY_QUERY: bottom-level structures of geometry, top-level
//! structures of their instances, builds and compaction.

use std::sync::Arc;

use super::copies::copying;
use super::render::index_format;
use super::*;
use crate::{
    GpuAccelerationStructureBuild, GpuBlasAabbGeometrySize, GpuBlasDescriptor,
    GpuBlasTriangleGeometrySize, GpuTlasDescriptor, GpuTlasInstance,
};

/// A BLAS and its device, which a compaction wait polls.
pub struct BlasEntry {
    blas: wgpu::Blas,
    device: wgpu::Device,
}

fn flags(value: Option<i32>) -> wgpu::AccelerationStructureFlags {
    wgpu::AccelerationStructureFlags::from_bits_truncate(value.unwrap_or(0) as u8)
}

fn geometry_flags(value: Option<i32>) -> wgpu::AccelerationStructureGeometryFlags {
    wgpu::AccelerationStructureGeometryFlags::from_bits_truncate(value.unwrap_or(0) as u8)
}

fn update_mode(value: Option<i32>) -> wgpu::AccelerationStructureUpdateMode {
    match value {
        Some(1) => wgpu::AccelerationStructureUpdateMode::PreferUpdate,
        _ => wgpu::AccelerationStructureUpdateMode::Build,
    }
}

fn triangle_size(
    size: &GpuBlasTriangleGeometrySize,
) -> Result<wgpu::BlasTriangleGeometrySizeDescriptor, String> {
    Ok(wgpu::BlasTriangleGeometrySizeDescriptor {
        vertex_format: vertex_format(size.vertexFormat),
        vertex_count: index(size.vertexCount, "vertexCount")?,
        index_format: size.indexFormat.map(index_format),
        index_count: size
            .indexCount
            .map(|count| index(count, "indexCount"))
            .transpose()?,
        flags: geometry_flags(size.flags),
    })
}

fn box_size(
    size: &GpuBlasAabbGeometrySize,
) -> Result<wgpu::BlasAABBGeometrySizeDescriptor, String> {
    Ok(wgpu::BlasAABBGeometrySizeDescriptor {
        primitive_count: index(size.primitiveCount, "primitiveCount")?,
        flags: geometry_flags(size.flags),
    })
}

fn one_kind<T, U>(triangles: &[T], boxes: &[U]) -> Result<(), String> {
    if !triangles.is_empty() && !boxes.is_empty() {
        return Err("a BLAS holds triangles or boxes, not both".into());
    }
    Ok(())
}

pub unsafe fn blas_create(device: i32, d: &GpuBlasDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let sizes = (|| {
        one_kind(&d.triangles, &d.aabbs)?;
        Ok::<_, String>(if d.aabbs.is_empty() {
            wgpu::BlasGeometrySizeDescriptors::Triangles {
                descriptors: d
                    .triangles
                    .iter()
                    .map(triangle_size)
                    .collect::<Result<_, _>>()?,
            }
        } else {
            wgpu::BlasGeometrySizeDescriptors::AABBs {
                descriptors: d.aabbs.iter().map(box_size).collect::<Result<_, _>>()?,
            }
        })
    })();
    let sizes = match sizes {
        Ok(sizes) => sizes,
        Err(message) => return refuse(&message),
    };
    let label = d.label.as_ref().map(caribou_abi::Rooted::get);
    let blas = entry.device.create_blas(
        &wgpu::CreateBlasDescriptor {
            label: label.as_ref().map(Text::as_str),
            flags: flags(d.flags),
            update_mode: update_mode(d.updateMode),
        },
        sizes,
    );
    BLASES.lock().unwrap().put(BlasEntry {
        blas,
        device: entry.device.clone(),
    })
}

pub unsafe fn blas_destroy(blas: i32) {
    BLASES.lock().unwrap().remove(blas);
}

pub unsafe fn blas_prepare_compaction(blas: i32) -> Future<()> {
    let Some(entry) = BLASES.lock().unwrap().get(blas) else {
        return rejected_future("the BLAS was destroyed");
    };
    let future = Future::new();
    let completion = Rooted::new(future);
    entry
        .blas
        .prepare_compaction_async(move |outcome| match outcome {
            Ok(()) => {
                completion.get().resolve(Value::null());
            }
            Err(_) => {
                completion.get().reject(
                    Text::new("the BLAS was rebuilt or destroyed before it could be compacted")
                        .value(),
                );
            }
        });
    drive_device(entry.device.clone());
    future
}

pub unsafe fn blas_ready_for_compaction(blas: i32) -> bool {
    find!(BLASES, blas, false).blas.ready_for_compaction()
}

pub unsafe fn queue_compact_blas(queue: i32, blas: i32) -> i32 {
    let queue = find!(QUEUES, queue, 0);
    let entry = find!(BLASES, blas, 0);
    let compacted = queue.compact_blas(&entry.blas);
    BLASES.lock().unwrap().put(BlasEntry {
        blas: compacted,
        device: entry.device.clone(),
    })
}

pub unsafe fn tlas_create(device: i32, d: &GpuTlasDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let max_instances = match index(d.maxInstances, "maxInstances") {
        Ok(max) => max,
        Err(message) => return refuse(&message),
    };
    let label = d.label.as_ref().map(caribou_abi::Rooted::get);
    let tlas = entry.device.create_tlas(&wgpu::CreateTlasDescriptor {
        label: label.as_ref().map(Text::as_str),
        max_instances,
        flags: flags(d.flags),
        update_mode: update_mode(d.updateMode),
    });
    TLASES.lock().unwrap().put(Mutex::new(tlas))
}

pub unsafe fn tlas_destroy(tlas: i32) {
    TLASES.lock().unwrap().remove(tlas);
}

pub unsafe fn tlas_max_instances(tlas: i32) -> i32 {
    find!(TLASES, tlas, 0).lock().unwrap().get().len() as i32
}

const IDENTITY_3X4: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];

fn instance(given: &GpuTlasInstance) -> Result<wgpu::TlasInstance, String> {
    let blas = BLASES
        .lock()
        .unwrap()
        .get(given.blas)
        .ok_or("the instance's BLAS was destroyed")?;
    let transform =
        if given.transform.is_empty() {
            IDENTITY_3X4
        } else {
            given.transform.as_slice().try_into().map_err(|_| {
                format!("a transform takes 12 values, not {}", given.transform.len())
            })?
        };
    let custom = given.customData.unwrap_or(0);
    if !(0..1 << 24).contains(&custom) {
        return Err(format!("customData {custom} does not fit in 24 bits"));
    }
    let mask = u8::try_from(given.mask.unwrap_or(0xFF))
        .map_err(|_| format!("mask {} does not fit in 8 bits", given.mask.unwrap_or(0)))?;
    Ok(wgpu::TlasInstance::new(
        &blas.blas,
        transform,
        custom as u32,
        mask,
    ))
}

fn set_slot(tlas: i32, at: i32, value: Option<wgpu::TlasInstance>) -> Result<(), String> {
    let tlas = TLASES
        .lock()
        .unwrap()
        .get(tlas)
        .ok_or("the TLAS was destroyed")?;
    let mut tlas = tlas.lock().unwrap();
    let max = tlas.get().len();
    let slot = usize::try_from(at)
        .ok()
        .and_then(|at| tlas.get_mut_single(at))
        .ok_or_else(|| format!("instance {at} is outside the TLAS's {max}"))?;
    *slot = value;
    Ok(())
}

pub unsafe fn tlas_set_instance(tlas: i32, at: i32, given: &GpuTlasInstance) {
    if let Err(message) = instance(given).and_then(|value| set_slot(tlas, at, Some(value))) {
        refuse(&message);
    }
}

pub unsafe fn tlas_clear_instance(tlas: i32, at: i32) {
    if let Err(message) = set_slot(tlas, at, None) {
        refuse(&message);
    }
}

/// One BLAS's geometry, resolved, so the build can borrow it.
struct Triangles {
    size: wgpu::BlasTriangleGeometrySizeDescriptor,
    vertices: Arc<wgpu::Buffer>,
    first_vertex: u32,
    stride: u64,
    indices: Option<Arc<wgpu::Buffer>>,
    first_index: Option<u32>,
    transform: Option<Arc<wgpu::Buffer>>,
    transform_offset: Option<u64>,
}

struct Boxes {
    size: wgpu::BlasAABBGeometrySizeDescriptor,
    buffer: Arc<wgpu::Buffer>,
    stride: u64,
    primitive_offset: u32,
}

struct Built {
    blas: Arc<BlasEntry>,
    triangles: Vec<Triangles>,
    boxes: Vec<Boxes>,
}

fn buffer(handle: i32) -> Result<Arc<wgpu::Buffer>, String> {
    BUFFERS
        .lock()
        .unwrap()
        .get(handle)
        .ok_or_else(|| "a geometry buffer was destroyed".into())
}

fn optional_buffer(handle: Option<i32>) -> Result<Option<Arc<wgpu::Buffer>>, String> {
    handle.map(buffer).transpose()
}

fn resolve_builds(build: &GpuAccelerationStructureBuild) -> Result<Vec<Built>, String> {
    build
        .blases
        .iter()
        .map(|entry| {
            one_kind(&entry.triangles, &entry.aabbs)?;
            Ok(Built {
                blas: BLASES
                    .lock()
                    .unwrap()
                    .get(entry.blas)
                    .ok_or("a BLAS to build was destroyed")?,
                triangles: entry
                    .triangles
                    .iter()
                    .map(|g| {
                        Ok(Triangles {
                            size: triangle_size(&g.size)?,
                            vertices: buffer(g.vertexBuffer)?,
                            first_vertex: index(g.firstVertex.unwrap_or(0), "firstVertex")?,
                            stride: size(g.vertexStride, "vertexStride")?,
                            indices: optional_buffer(g.indexBuffer)?,
                            first_index: g
                                .firstIndex
                                .map(|first| index(first, "firstIndex"))
                                .transpose()?,
                            transform: optional_buffer(g.transformBuffer)?,
                            transform_offset: g
                                .transformBufferOffset
                                .map(|offset| size(offset, "transformBufferOffset"))
                                .transpose()?,
                        })
                    })
                    .collect::<Result<_, String>>()?,
                boxes: entry
                    .aabbs
                    .iter()
                    .map(|g| {
                        Ok(Boxes {
                            size: box_size(&g.size)?,
                            buffer: buffer(g.aabbBuffer)?,
                            stride: size(g.stride, "stride")?,
                            primitive_offset: index(
                                g.primitiveOffset.unwrap_or(0),
                                "primitiveOffset",
                            )?,
                        })
                    })
                    .collect::<Result<_, String>>()?,
            })
        })
        .collect()
}

fn resolve_tlases(
    build: &GpuAccelerationStructureBuild,
) -> Result<Vec<Arc<Mutex<wgpu::Tlas>>>, String> {
    let mut tlases: Vec<Arc<Mutex<wgpu::Tlas>>> = Vec::with_capacity(build.tlases.len());
    for &handle in &build.tlases {
        let tlas = TLASES
            .lock()
            .unwrap()
            .get(handle)
            .ok_or("a TLAS to build was destroyed")?;
        if tlases.iter().any(|seen| Arc::ptr_eq(seen, &tlas)) {
            return Err("a TLAS is listed twice in one build".into());
        }
        tlases.push(tlas);
    }
    Ok(tlases)
}

pub unsafe fn encoder_build_acceleration_structures(
    encoder: i32,
    build: &GpuAccelerationStructureBuild,
) {
    let resolved = resolve_builds(build).and_then(|built| Ok((built, resolve_tlases(build)?)));
    let (built, tlases) = match resolved {
        Ok(resolved) => resolved,
        Err(message) => {
            refuse(&message);
            return;
        }
    };
    let entries: Vec<wgpu::BlasBuildEntry> = built
        .iter()
        .map(|b| wgpu::BlasBuildEntry {
            blas: &b.blas.blas,
            geometry: if b.boxes.is_empty() {
                wgpu::BlasGeometries::TriangleGeometries(
                    b.triangles
                        .iter()
                        .map(|t| wgpu::BlasTriangleGeometry {
                            size: &t.size,
                            vertex_buffer: &t.vertices,
                            first_vertex: t.first_vertex,
                            vertex_stride: t.stride,
                            index_buffer: t.indices.as_deref(),
                            first_index: t.first_index,
                            transform_buffer: t.transform.as_deref(),
                            transform_buffer_offset: t.transform_offset,
                        })
                        .collect(),
                )
            } else {
                wgpu::BlasGeometries::AabbGeometries(
                    b.boxes
                        .iter()
                        .map(|g| wgpu::BlasAabbGeometry {
                            size: &g.size,
                            stride: g.stride,
                            aabb_buffer: &g.buffer,
                            primitive_offset: g.primitive_offset,
                        })
                        .collect(),
                )
            },
        })
        .collect();
    let locked: Vec<_> = tlases.iter().map(|tlas| tlas.lock().unwrap()).collect();
    copying(encoder, |encoder| {
        encoder.build_acceleration_structures(&entries, locked.iter().map(|tlas| &**tlas));
        Ok(())
    });
}
