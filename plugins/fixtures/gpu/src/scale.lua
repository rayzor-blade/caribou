-- Lua driving the gpu plugin with an HXSL shader the Haxe program compiled:
-- the shader class, its WGSL and its layout constants cross like any Haxe
-- class; Lua's strings are the bytes it uploads, each a read-only buffer
-- over the string itself, and a core Buffer is what it reads back into,
-- which string.unpack reads in place.
local ScaleValues = require("haxe.ScaleValues").ScaleValues
local Buffer = require("core.Buffer").Buffer
local BufferUsage = require("gpu.BufferUsage").BufferUsage
local GpuBufferDescriptor = require("gpu.GpuBufferDescriptor").GpuBufferDescriptor
local GpuComputePipelineDescriptor =
  require("gpu.GpuComputePipelineDescriptor").GpuComputePipelineDescriptor
local GpuProgrammableStage = require("gpu.GpuProgrammableStage").GpuProgrammableStage
local GpuBindGroupEntry = require("gpu.GpuBindGroupEntry").GpuBindGroupEntry
local GpuBindGroupDescriptor = require("gpu.GpuBindGroupDescriptor").GpuBindGroupDescriptor

local Scale = {}

-- Scales 1, 2, 3 and 4 by 3 through the imported helper, which adds 1.
function Scale.run(device, queue)
  local shader = device:createShader(ScaleValues.WGSL)
  local pipeline = device:createComputePipeline(
    GpuComputePipelineDescriptor(GpuProgrammableStage(shader)))

  local usage = BufferUsage.STORAGE() | BufferUsage.COPY_DST() | BufferUsage.COPY_SRC()
  local values = device:createBuffer(GpuBufferDescriptor(16, usage))
  queue:writeBuffer(values, 0, string.pack("<ffff", 1, 2, 3, 4), 16)

  -- The params block as floats: each offset the shader reports is in bytes.
  local floats = {}
  for i = 1, ScaleValues.PARAMS_SIZE // 4 do floats[i] = 0 end
  floats[ScaleValues.PARAMS_gain // 4 + 1] = 3
  local uniforms = device:createBuffer(GpuBufferDescriptor(ScaleValues.PARAMS_SIZE,
    BufferUsage.UNIFORM() | BufferUsage.COPY_DST()))
  queue:writeBuffer(uniforms, 0, string.pack("<" .. string.rep("f", #floats), table.unpack(floats)),
    ScaleValues.PARAMS_SIZE)

  local paramsEntry = GpuBindGroupEntry(ScaleValues.PARAMS_BINDING)
  paramsEntry:resourceBuffer(uniforms)
  local valuesEntry = GpuBindGroupEntry(ScaleValues.BUFFER_values)
  valuesEntry:resourceBuffer(values)
  local descriptor = GpuBindGroupDescriptor(pipeline:getBindGroupLayout(ScaleValues.PARAMS_GROUP))
  descriptor:addEntries(paramsEntry)
  descriptor:addEntries(valuesEntry)
  local group = device:createBindGroup(descriptor)

  local readback = device:createBuffer(GpuBufferDescriptor(16,
    BufferUsage.MAP_READ() | BufferUsage.COPY_DST()))
  local encoder = device:encoder()
  encoder:computeBegin()
  encoder:computeSetPipeline(pipeline)
  encoder:computeSetBindGroup(0, group)
  encoder:computeDispatch(1, 1, 1)
  encoder:computeEnd()
  encoder:copyBuffer(values, 0, readback, 0, 16)
  encoder:submit(queue)
  device:queueWorkDone(queue):await()
  device:mapBuffer(readback, 0, 16):await()
  local out = Buffer(16)
  readback:copyOut(0, out, 16)
  readback:unmap()
  local a, b, c, d = string.unpack("<ffff", out)
  return string.format("%d,%d,%d,%d", a, b, c, d)
end

return { Scale = Scale }
