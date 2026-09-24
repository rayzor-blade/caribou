//! Query sets: occlusion, timestamp and pipeline statistics queries, and
//! the timestamps an encoder writes.

use super::*;
use crate::GpuQuerySetDescriptor;

/// A query set and what it was made as, which wgpu does not report back.
pub struct QuerySetEntry {
    pub set: wgpu::QuerySet,
    count: u32,
    kind: i32,
}

pub unsafe fn query_set_create(device: i32, descriptor: &GpuQuerySetDescriptor) -> i32 {
    let entry = find!(DEVICES, device, 0);
    let count = match index(descriptor.count, "query count") {
        Ok(count) => count,
        Err(message) => return refuse(&message),
    };
    // The IDL's order, then wgpu's pipeline statistics.
    let ty = match (descriptor.r#type, descriptor.pipelineStatistics) {
        (0, _) => wgpu::QueryType::Occlusion,
        (1, _) => wgpu::QueryType::Timestamp,
        (2, Some(counted)) => wgpu::QueryType::PipelineStatistics(
            wgpu::PipelineStatisticsTypes::from_bits_truncate(counted as u8),
        ),
        (2, None) => {
            return refuse("a pipeline statistics query set needs pipelineStatistics");
        }
        _ => return refuse("unknown query type"),
    };
    let label = descriptor.label.as_ref().map(caribou_abi::Rooted::get);
    let set = entry.device.create_query_set(&wgpu::QuerySetDescriptor {
        label: label.as_ref().map(Text::as_str),
        ty,
        count,
    });
    QUERY_SETS.lock().unwrap().put(QuerySetEntry {
        set,
        count,
        kind: descriptor.r#type,
    })
}

pub unsafe fn query_set_destroy(set: i32) {
    QUERY_SETS.lock().unwrap().remove(set);
}

pub unsafe fn query_set_count(set: i32) -> i32 {
    find!(QUERY_SETS, set, 0).count as i32
}

pub unsafe fn query_set_type(set: i32) -> i32 {
    find!(QUERY_SETS, set, 0).kind
}

/// Nanoseconds per tick of a timestamp query on this queue.
pub unsafe fn queue_timestamp_period(queue: i32) -> f64 {
    f64::from(find!(QUEUES, queue, 0.0).get_timestamp_period())
}

fn with_encoder(handle: i32, body: impl FnOnce(&mut wgpu::CommandEncoder)) {
    let Some(entry) = ENCODERS.lock().unwrap().get(handle) else {
        return;
    };
    let mut held = entry.lock().unwrap();
    if held.pass.is_some() || held.compute.is_some() {
        return host::raise(ErrorKind::Runtime, "a pass is open on this encoder");
    }
    if let Some(encoder) = held.encoder.as_mut() {
        body(encoder);
    }
}

pub unsafe fn encoder_write_timestamp(encoder: i32, set: i32, index_of: i32) {
    let set = find!(QUERY_SETS, set);
    let query = match index(index_of, "query index") {
        Ok(query) => query,
        Err(message) => return host::raise(ErrorKind::Type, &message),
    };
    with_encoder(encoder, |encoder| encoder.write_timestamp(&set.set, query));
}

/// Results of `count` queries from `first`, as 64-bit values, into a
/// buffer made with QUERY_RESOLVE usage.
pub unsafe fn encoder_resolve_query_set(
    encoder: i32,
    set: i32,
    first: i32,
    count: i32,
    destination: i32,
    offset: i64,
) {
    let set = find!(QUERY_SETS, set);
    let destination = find!(BUFFERS, destination);
    let (Ok(first), Ok(count), Ok(offset)) = (
        index(first, "first query"),
        index(count, "query count"),
        size(offset, "offset"),
    ) else {
        return host::raise(ErrorKind::Type, "negative query range or offset");
    };
    with_encoder(encoder, |encoder| {
        encoder.resolve_query_set(
            &set.set,
            first..first.saturating_add(count),
            &destination,
            offset,
        )
    });
}
