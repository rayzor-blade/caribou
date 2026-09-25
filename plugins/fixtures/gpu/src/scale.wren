// Wren driving the gpu plugin with an HXSL shader the Haxe program compiled:
// the shader class, its WGSL and its layout constants cross like any Haxe
// class, and Wren's typed arrays are the buffers it uploads and reads back.
import "haxe:ScaleValues" for ScaleValues
import "gpu:GpuDevice" for GpuDevice
import "gpu:GpuQueue" for GpuQueue
import "gpu:GpuBufferDescriptor" for GpuBufferDescriptor
import "gpu:BufferUsage" for BufferUsage
import "gpu:GpuComputePipelineDescriptor" for GpuComputePipelineDescriptor
import "gpu:GpuProgrammableStage" for GpuProgrammableStage
import "gpu:GpuBindGroupEntry" for GpuBindGroupEntry
import "gpu:GpuBindGroupDescriptor" for GpuBindGroupDescriptor

class Scale {
  // Scales 1, 2, 3 and 4 by 3 through the imported helper, which adds 1.
  #export = "run(device: GpuDevice, queue: GpuQueue) -> String"
  static run(device, queue) {
    var shader = device.createShader(ScaleValues.WGSL)
    var pipeline = device.createComputePipeline(GpuComputePipelineDescriptor.new(GpuProgrammableStage.new(shader)))

    var values = device.createBuffer(GpuBufferDescriptor.new(16, BufferUsage.STORAGE() | BufferUsage.COPY_DST() | BufferUsage.COPY_SRC()))
    queue.writeBuffer(values, 0, Float32Array.fromList([1, 2, 3, 4]), 16)

    // The params buffer as floats: each offset the shader reports is in bytes.
    var params = Float32Array.new(ScaleValues.PARAMS_SIZE / 4)
    params[ScaleValues.PARAMS_gain / 4] = 3
    var uniforms = device.createBuffer(GpuBufferDescriptor.new(ScaleValues.PARAMS_SIZE, BufferUsage.UNIFORM() | BufferUsage.COPY_DST()))
    queue.writeBuffer(uniforms, 0, params, ScaleValues.PARAMS_SIZE)

    var paramsEntry = GpuBindGroupEntry.new(ScaleValues.PARAMS_BINDING)
    paramsEntry.resourceBuffer(uniforms)
    var valuesEntry = GpuBindGroupEntry.new(ScaleValues.BUFFER_values)
    valuesEntry.resourceBuffer(values)
    var descriptor = GpuBindGroupDescriptor.new(pipeline.getBindGroupLayout(ScaleValues.PARAMS_GROUP))
    descriptor.addEntries(paramsEntry)
    descriptor.addEntries(valuesEntry)
    var group = device.createBindGroup(descriptor)

    var readback = device.createBuffer(GpuBufferDescriptor.new(16, BufferUsage.MAP_READ() | BufferUsage.COPY_DST()))
    var encoder = device.encoder()
    encoder.computeBegin()
    encoder.computeSetPipeline(pipeline)
    encoder.computeSetBindGroup(0, group)
    encoder.computeDispatch(1, 1, 1)
    encoder.computeEnd()
    encoder.copyBuffer(values, 0, readback, 0, 16)
    encoder.submit(queue)
    device.queueWorkDone(queue).await()
    device.mapBuffer(readback, 0, 16).await()
    var out = Float32Array.new(4)
    readback.copyOut(0, out, 16)
    readback.unmap()
    return out.toList.join(",")
  }
}
