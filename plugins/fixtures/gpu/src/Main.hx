import gpu.GpuInstance;
import gpu.GpuBindings;
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
        device.destroy();
        adapter.destroy();
        instance.destroy();
        Sys.println("gpu compute ok");
    }
}
