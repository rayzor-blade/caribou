import gpu.GpuInstance;
import gpu.GpuBindings;
import gpu.GpuBindGroupDescriptor;
import gpu.GpuBindGroupEntry;
import gpu.GpuBindGroupLayoutDescriptor;
import gpu.GpuBindGroupLayoutEntry;
import gpu.GpuBufferBinding;
import gpu.GpuBufferBindingLayout;
import gpu.GpuComputePipelineDescriptor;
import gpu.GpuDevice;
import gpu.GpuPipelineLayoutDescriptor;
import gpu.GpuProgrammableStage;
import gpu.GpuQueue;
import gpu.GpuSamplerBindingLayout;
import gpu.ShaderStage;
import gpu.GpuBufferDescriptor;
import gpu.GpuDeviceDescriptor;
import gpu.BufferUsage;
import gpu.Feature;
import gpu.Limit;
import gpu.Power;

class Main {
    static function check(ok:Bool, message:String) {
        if (!ok) throw message;
    }
    /** Throws, or the check fails with `message`. */
    static function refused(message:String, attempt:() -> Void) {
        var caught = false;
        try attempt() catch (_:Dynamic) caught = true;
        check(caught, message);
    }

    /**
        Explicit layouts: one bind group layout with a dynamic storage window
        and a uniform range, a pipeline layout over it, a compute pipeline
        with an overridable constant, and one bind group dispatched at two
        dynamic offsets inside one compute pass.
    **/
    static function explicitLayouts(device:GpuDevice, queue:GpuQueue) {
        // 256 is the most any adapter may demand for either offset alignment.
        var window = 256;
        var values = haxe.io.Bytes.alloc(window * 2);
        for (i in 0...4) {
            values.setInt32(i * 4, i + 1);
            values.setInt32(window + i * 4, (i + 1) * 10);
        }
        var storage = device.createBuffer(new GpuBufferDescriptor(window * 2,
            BufferUsage.STORAGE() | BufferUsage.COPY_DST() | BufferUsage.COPY_SRC()));
        queue.writeBuffer(storage, 0, values, values.length);
        var params = haxe.io.Bytes.alloc(window * 2);
        params.setInt32(window, 5);
        var uniforms = device.createBuffer(new GpuBufferDescriptor(window * 2,
            BufferUsage.UNIFORM() | BufferUsage.COPY_DST()));
        queue.writeBuffer(uniforms, 0, params, params.length);
        var readback = device.createBuffer(new GpuBufferDescriptor(window * 2,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));

        var dynamicWindow = new GpuBufferBindingLayout();
        dynamicWindow.type(Storage);
        dynamicWindow.hasDynamicOffset(true);
        dynamicWindow.minBindingSize(16);
        var first = new GpuBindGroupLayoutEntry(0, ShaderStage.COMPUTE());
        first.buffer(dynamicWindow);
        var second = new GpuBindGroupLayoutEntry(1, ShaderStage.COMPUTE());
        second.buffer(new GpuBufferBindingLayout());
        var layoutDescriptor = new GpuBindGroupLayoutDescriptor();
        layoutDescriptor.label("windows");
        layoutDescriptor.addEntries(first);
        layoutDescriptor.addEntries(second);
        var groupLayout = device.createBindGroupLayout(layoutDescriptor);
        check(groupLayout.valid(), "bind group layout was not created");

        var ambiguous = new GpuBindGroupLayoutEntry(2, ShaderStage.COMPUTE());
        ambiguous.buffer(new GpuBufferBindingLayout());
        ambiguous.sampler(new GpuSamplerBindingLayout());
        var twoLayouts = new GpuBindGroupLayoutDescriptor();
        twoLayouts.addEntries(ambiguous);
        refused("an entry with two layouts was accepted", () -> device.createBindGroupLayout(twoLayouts));

        var pipelineLayoutDescriptor = new GpuPipelineLayoutDescriptor();
        pipelineLayoutDescriptor.addBindGroupLayouts(groupLayout);
        var pipelineLayout = device.createPipelineLayout(pipelineLayoutDescriptor);
        var shader = device.createShader('
            struct Params { scale: u32, a: u32, b: u32, c: u32 };
            @group(0) @binding(0) var<storage, read_write> values: array<u32, 4>;
            @group(0) @binding(1) var<uniform> params: Params;
            override bias: u32 = 0u;
            @compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                values[id.x] = values[id.x] * params.scale + bias;
            }');
        var stage = new GpuProgrammableStage(shader);
        stage.entryPoint("main");
        stage.addConstants("bias", 1);
        var pipelineDescriptor = new GpuComputePipelineDescriptor(stage);
        pipelineDescriptor.layout(pipelineLayout);
        var pipeline = device.createComputePipeline(pipelineDescriptor);
        var inferred = pipeline.getBindGroupLayout(0);
        check(inferred.valid(), "pipeline did not report its bind group layout");

        var windowRange = new GpuBufferBinding(storage);
        windowRange.size(16);
        var paramsRange = new GpuBufferBinding(uniforms);
        paramsRange.offset(window);
        paramsRange.size(16);
        var windowEntry = new GpuBindGroupEntry(0);
        windowEntry.resourceBufferBinding(windowRange);
        var paramsEntry = new GpuBindGroupEntry(1);
        paramsEntry.resourceBufferBinding(paramsRange);
        var groupDescriptor = new GpuBindGroupDescriptor(groupLayout);
        groupDescriptor.addEntries(windowEntry);
        groupDescriptor.addEntries(paramsEntry);
        var group = device.createBindGroup(groupDescriptor);

        var unset = new GpuBindGroupDescriptor(groupLayout);
        unset.addEntries(new GpuBindGroupEntry(0));
        refused("an entry without a resource was accepted", () -> device.createBindGroup(unset));

        var offsets = haxe.io.Bytes.alloc(8);
        offsets.setInt32(0, 0);
        offsets.setInt32(4, window);
        var encoder = device.encoder();
        encoder.computeBegin();
        encoder.computeSetPipeline(pipeline);
        encoder.computeSetBindGroupOffsets(0, group, offsets, 0, 1);
        encoder.computeDispatch(4, 1, 1);
        encoder.computeSetBindGroupOffsets(0, group, offsets, 1, 1);
        encoder.computeDispatch(4, 1, 1);
        refused("offsets past the shared buffer were read",
            () -> encoder.computeSetBindGroupOffsets(0, group, offsets, 2, 1));
        encoder.computeEnd();
        encoder.copyBuffer(storage, 0, readback, 0, window * 2);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBuffer(readback, 0, window * 2).await();
        var output = haxe.io.Bytes.alloc(window * 2);
        check(readback.copyOut(0, output, output.length), "layout readback failed");
        for (i in 0...4) {
            check(output.getInt32(i * 4) == (i + 1) * 5 + 1, "first dynamic window mismatch");
            check(output.getInt32(window + i * 4) == (i + 1) * 50 + 1, "second dynamic window mismatch");
        }
        readback.unmap();
        check(device.takeError() == null, "GPU validation error in explicit layouts");

        group.destroy();
        inferred.destroy();
        pipeline.destroy();
        shader.destroy();
        pipelineLayout.destroy();
        groupLayout.destroy();
        check(!groupLayout.valid(), "destroyed bind group layout remains live");
        storage.destroy();
        uniforms.destroy();
        readback.destroy();
        Sys.println("gpu explicit layouts ok");
    }

    static function main() {
        // Caribou generates every gpu.* type from the plugin's own schema.
        var instance = new GpuInstance();
        var adapter = instance.requestAdapter(HighPerformance).await();
        if (!adapter.valid()) {
            instance.destroy();
            throw "No GPU adapter is available";
        }
        Sys.println('GPU: ${adapter.name()} (${adapter.backend()})');
        Sys.println('shader-f16: ${adapter.supports(ShaderF16)}');
        check(adapter.limit(MaxBufferSize) >= 16, "adapter buffer limit is too small");
        var requested = new GpuDeviceDescriptor();
        requested.addRequiredLimits(MaxBindGroups, 4);
        var device = adapter.requestDeviceWith(requested).await();
        check(device.valid(), "device request failed");
        check(device.limit(MaxBindGroups) >= 4, "negotiated bind-group limit missing");
        var queue = device.queue();
        var data = haxe.io.Bytes.alloc(16);
        for (i in 0...4) data.setInt32(i * 4, i + 1);
        var storageDescriptor = new GpuBufferDescriptor(16,
            BufferUsage.STORAGE() | BufferUsage.COPY_DST() | BufferUsage.COPY_SRC());
        storageDescriptor.label("compute storage");
        storageDescriptor.mappedAtCreation(false);
        var storage = device.createBuffer(storageDescriptor);
        var readback = device.createBuffer(new GpuBufferDescriptor(16,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));
        queue.writeBuffer(storage, 0, data, data.length);
        // Input Text is UTF-8, including this comment; no UTF-16 ABI helpers.
        var shader = device.createShader('/* é文 */
            @group(0) @binding(0) var<storage, read_write> values: array<u32>;
            @compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                values[id.x] = values[id.x] * 3u;
            }');
        var pipeline = device.computePipeline(shader, "main");
        var bindings = new GpuBindings();
        bindings.buffer(storage);
        var group = device.bindGroup(pipeline, 0, bindings);
        bindings.destroy();
        var encoder = device.encoder();
        encoder.compute(pipeline, group, 4, 1, 1);
        encoder.copyBuffer(storage, 0, readback, 0, 16);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBuffer(readback, 0, 16).await();
        var output = haxe.io.Bytes.alloc(16);
        hl.Gc.major();
        check(readback.copyOut(0, output, output.length), "readback failed");
        for (i in 0...4) check(output.getInt32(i * 4) == (i + 1) * 3, "compute result mismatch");
        var caught = false;
        try { queue.writeBuffer(storage, 0, data, data.length + 4); }
        catch (_:Dynamic) { caught = true; }
        check(caught, "upload exceeded source buffer");
        caught = false;
        try { readback.copyOut(0, output, output.length + 4); }
        catch (_:Dynamic) { caught = true; }
        check(caught, "readback exceeded destination buffer");
        readback.unmap();
        check(device.takeError() == null, "GPU validation error");
        group.destroy();
        pipeline.destroy();
        shader.destroy();
        storage.destroy();
        readback.destroy();
        check(!storage.valid(), "destroyed buffer remains live");
        explicitLayouts(device, queue);
        device.destroy();
        adapter.destroy();
        instance.destroy();
        Sys.println("gpu compute ok");
    }
}
