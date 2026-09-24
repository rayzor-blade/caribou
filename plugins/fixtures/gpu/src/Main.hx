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
import gpu.GpuAdapter;
import gpu.GpuColor;
import gpu.GpuColorTargetState;
import gpu.GpuExtent3D;
import gpu.GpuFragmentState;
import gpu.GpuOrigin3D;
import gpu.GpuQuerySetDescriptor;
import gpu.GpuRenderBundleEncoderDescriptor;
import gpu.GpuRenderPassColorAttachment;
import gpu.GpuRenderPassDescriptor;
import gpu.GpuRenderPassTimestampWrites;
import gpu.GpuRenderPipelineDescriptor;
import gpu.GpuTexelCopyBufferInfo;
import gpu.GpuTexelCopyBufferLayout;
import gpu.GpuTexelCopyTextureInfo;
import gpu.GpuTextureDescriptor;
import gpu.GpuVertexState;
import gpu.TextureUsage;
import gpu.TextureFormatFeature;
import gpu.NativeFeature;
import gpu.CompilationMessageType;
import gpu.DeviceLostReason;
import gpu.ErrorFilter;
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
    /** A multi-line message's last non-empty line, where wgpu puts the cause. */
    static function lastLine(message:String):String {
        var lines = [for (line in message.split("\n")) if (StringTools.trim(line) != "") StringTools.trim(line)];
        return lines.length == 0 ? "" : lines[lines.length - 1];
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

    /**
        Offscreen rendering from WebGPU's descriptors: a pipeline created
        asynchronously with a fragment constant, drawn through a render
        bundle inside a pass that also counts occlusion and, where the
        adapter has them, writes timestamps. Then copies with full texel
        descriptions, error scopes, compilation messages and device loss.
    **/
    static function renderingQueriesAndDiagnostics(adapter:GpuAdapter, device:GpuDevice, queue:GpuQueue,
            timestamps:Bool) {
        var size = new GpuExtent3D(4);
        size.height(4);
        var target = device.texture(new GpuTextureDescriptor(size, Rgba8unorm,
            TextureUsage.RENDER_ATTACHMENT() | TextureUsage.COPY_SRC() | TextureUsage.COPY_DST()));
        var shader = device.createShader('
            override red: f32 = 0.0;
            @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                let x = f32(i32(i) / 2) * 4.0 - 1.0;
                let y = f32(i32(i) % 2) * 4.0 - 1.0;
                return vec4<f32>(x, y, 0.0, 1.0);
            }
            @fragment fn fs() -> @location(0) vec4<f32> {
                return vec4<f32>(red, 0.5, 0.25, 1.0);
            }');
        var vertex = new GpuVertexState(shader);
        vertex.entryPoint("vs");
        var fragment = new GpuFragmentState(shader);
        fragment.entryPoint("fs");
        fragment.addConstants("red", 1);
        fragment.addTargets(new GpuColorTargetState(Rgba8unorm));
        var pipelineDescriptor = new GpuRenderPipelineDescriptor(vertex);
        pipelineDescriptor.fragment(fragment);
        var pipeline = device.createRenderPipelineAsync(pipelineDescriptor).await();
        check(pipeline.valid(), "asynchronous render pipeline failed");

        var bundleDescriptor = new GpuRenderBundleEncoderDescriptor();
        bundleDescriptor.addColorFormats(Rgba8unorm);
        var recording = device.createRenderBundleEncoder(bundleDescriptor);
        recording.setPipeline(pipeline);
        recording.draw(3, 1, 0, 0);
        var bundle = recording.finish();
        check(!recording.valid(), "a finished bundle encoder remains live");

        var occlusion = device.createQuerySet(new GpuQuerySetDescriptor(Occlusion, 1));
        check(occlusion.count() == 1 && Type.enumEq(occlusion.queryType(), Occlusion), "query set shape");
        var times = timestamps ? device.createQuerySet(new GpuQuerySetDescriptor(Timestamp, 2)) : null;
        // Query results resolve at 256-byte aligned offsets.
        var resolved = device.createBuffer(new GpuBufferDescriptor(512,
            BufferUsage.QUERY_RESOLVE() | BufferUsage.COPY_SRC()));
        var queries = device.createBuffer(new GpuBufferDescriptor(512,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));
        var pixels = device.createBuffer(new GpuBufferDescriptor(256 * 4,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));

        var colour = new GpuRenderPassColorAttachment(Clear, Store);
        colour.viewTexture(target);
        colour.clearValue(new GpuColor(0, 0, 1, 1));
        var pass = new GpuRenderPassDescriptor();
        pass.addColorAttachments(colour);
        pass.occlusionQuerySet(occlusion);
        if (timestamps) {
            var writes = new GpuRenderPassTimestampWrites(times);
            writes.beginningOfPassWriteIndex(0);
            writes.endOfPassWriteIndex(1);
            pass.timestampWrites(writes);
        }
        var encoder = device.encoder();
        encoder.beginRenderPass(pass);
        encoder.renderBeginOcclusionQuery(0);
        encoder.renderExecuteBundle(bundle);
        encoder.renderEndOcclusionQuery();
        encoder.renderEnd();
        encoder.resolveQuerySet(occlusion, 0, 1, resolved, 0);
        if (timestamps) encoder.resolveQuerySet(times, 0, 2, resolved, 256);
        encoder.copyBuffer(resolved, 0, queries, 0, 512);
        var from = new GpuTexelCopyTextureInfo(target);
        var into = new GpuTexelCopyBufferInfo(pixels);
        into.bytesPerRow(256);
        into.rowsPerImage(4);
        encoder.copyTextureToBufferWith(from, into, size);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBuffer(pixels, 0, 256 * 4).await();
        var image = haxe.io.Bytes.alloc(256 * 4);
        check(pixels.copyOut(0, image, image.length), "pixel readback failed");
        for (row in 0...4) for (column in 0...4) {
            var at = row * 256 + column * 4;
            check(image.get(at) == 255, "red channel is the overridden constant");
            check(Math.abs(image.get(at + 1) - 128) <= 1, "green channel");
            check(image.get(at + 3) == 255, "alpha channel");
        }
        device.mapBuffer(queries, 0, 512).await();
        var counts = haxe.io.Bytes.alloc(512);
        check(queries.copyOut(0, counts, counts.length), "query readback failed");
        check(counts.getInt32(0) > 0, "occlusion counted no samples");
        if (timestamps) {
            var begin = counts.getInt64(256);
            var end = counts.getInt64(264);
            check(haxe.Int64.compare(end, begin) >= 0, "timestamps run backwards");
            Sys.println('render pass: ${haxe.Int64.toStr(end - begin)} ticks of ${queue.timestampPeriod()} ns');
        }
        pixels.unmap();
        queries.unmap();

        // A 2x2 upload at (1, 1) through full texel descriptions.
        var patch = haxe.io.Bytes.alloc(8 * 2);
        for (i in 0...patch.length) patch.set(i, 200);
        var patchTarget = new GpuTexelCopyTextureInfo(target);
        var origin = new GpuOrigin3D();
        origin.x(1);
        origin.y(1);
        patchTarget.origin(origin);
        var layout = new GpuTexelCopyBufferLayout();
        layout.bytesPerRow(8);
        var patchSize = new GpuExtent3D(2);
        patchSize.height(2);
        queue.writeTextureWith(patchTarget, patch, layout, patchSize);
        encoder = device.encoder();
        var whole = new GpuTexelCopyBufferInfo(pixels);
        whole.bytesPerRow(256);
        encoder.copyTextureToBufferWith(new GpuTexelCopyTextureInfo(target), whole, size);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBuffer(pixels, 0, 256 * 4).await();
        check(pixels.copyOut(0, image, image.length), "pixel readback failed");
        check(image.get(256 + 4) == 200 && image.get(256 * 2 + 8) == 200, "the upload is not at (1, 1)");
        check(image.get(0) == 255 && image.get(256 * 3 + 12) == 255, "the upload spilled outside");
        pixels.unmap();

        check(device.takeError() == null, "GPU validation error in rendering");

        // An error scope catches what the device would otherwise report.
        device.pushErrorScope(Validation);
        device.createBuffer(new GpuBufferDescriptor(16, BufferUsage.MAP_READ() | BufferUsage.STORAGE()));
        var caught = device.popErrorScope().await();
        check(caught != null && Type.enumEq(caught.filter(), Validation), "the scope missed a validation error");
        Sys.println('scoped: ${lastLine(caught.message())}');
        device.pushErrorScope(Validation);
        check(device.popErrorScope().await() == null, "an empty scope reported an error");

        device.pushErrorScope(Validation);
        var broken = device.createShader("@compute @workgroup_size(1) fn main() { let x: u32 = ; }");
        device.popErrorScope().await();
        var info = broken.getCompilationInfo().await();
        check(info.messageCount() > 0, "no compilation messages");
        check(Type.enumEq(info.messageType(0), CompilationMessageType.Error), "the message is not an error");
        check(info.lineNum(0) == 1, "the message has no line");
        Sys.println('compiler: line ${info.lineNum(0)}:${info.linePos(0)} ${StringTools.trim(info.message(0)).split("\n")[0]}');

        check((adapter.textureFormatFeatures(Rgba8unorm) & TextureFormatFeature.FILTERABLE()) != 0,
            "rgba8unorm is not filterable");
        check((adapter.textureFormatUsages(Rgba8unorm) & TextureUsage.RENDER_ATTACHMENT()) != 0,
            "rgba8unorm cannot be rendered to");

        bundle.destroy();
        pipeline.destroy();
        shader.destroy();
        broken.destroy();
        occlusion.destroy();
        if (timestamps) times.destroy();
        target.destroy();
        resolved.destroy();
        queries.destroy();
        pixels.destroy();
        Sys.println("gpu rendering and diagnostics ok");
    }

    /** A device's lost future resolves when it is destroyed. **/
    static function deviceLoss(adapter:GpuAdapter) {
        var device = adapter.requestDevice().await();
        var lost = device.lost();
        check(!lost.ready(), "a live device reads as lost");
        device.destroy();
        var info = lost.await();
        check(Type.enumEq(info.reason(), DeviceLostReason.Destroyed), "loss reason is not destruction");
        Sys.println("gpu device loss ok");
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
        var timestamps = adapter.supports(TimestampQuery);
        if (timestamps) requested.addRequiredFeatures(TimestampQuery);
        var native = 0;
        for (feature in Type.allEnums(NativeFeature)) if (adapter.supportsNative(feature)) native++;
        Sys.println('native features: ${native} of ${Type.allEnums(NativeFeature).length}');
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
        renderingQueriesAndDiagnostics(adapter, device, queue, timestamps);
        deviceLoss(adapter);
        device.destroy();
        adapter.destroy();
        instance.destroy();
        Sys.println("gpu compute ok");
    }
}
