import gpu.GpuInstance;
import gpu.GpuPassthroughEntryPoint;
import gpu.GpuPassthroughShaderDescriptor;
import gpu.GpuShaderModuleDescriptor;
import gpu.Backends;
import gpu.GpuInstanceDescriptor;
import gpu.GpuRequestAdapterOptions;
import gpu.InstanceFlag;
import gpu.MapMode;
import gpu.AccelerationStructureFlag;
import gpu.AccelerationStructureGeometryFlag;
import gpu.GpuAccelerationStructureBuild;
import gpu.GpuBindGroup;
import gpu.GpuBlasBuildEntry;
import gpu.GpuBlasDescriptor;
import gpu.GpuBlasTriangleGeometry;
import gpu.GpuBlasTriangleGeometrySize;
import gpu.GpuBuffer;
import gpu.GpuBufferArray;
import gpu.GpuEncoder;
import gpu.GpuExternalTextureDescriptor;
import gpu.GpuMeshPipelineDescriptor;
import gpu.GpuPipeline;
import gpu.GpuTexture;
import gpu.GpuTextureBindingLayout;
import gpu.GpuTextureViewArray;
import gpu.GpuTextureViewDescriptor;
import gpu.GpuTlasDescriptor;
import gpu.GpuTlasInstance;
import gpu.NativeLimit;
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
import gpu.TextureDimension;
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
        var careless = new GpuRenderPassColorAttachment(DontCare, Store);
        careless.viewTexture(target);
        var carelessPass = new GpuRenderPassDescriptor();
        carelessPass.addColorAttachments(careless);
        refused("a DontCare load ran without the device's opt-in",
            () -> device.encoder().beginRenderPass(carelessPass));

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

    /** Runs one workgroup of `pipeline` with `group` bound at 0 and reads `out` back. */
    static function dispatchOnce(device:GpuDevice, queue:GpuQueue, pipeline:GpuPipeline, group:GpuBindGroup,
            out:GpuBuffer, size:Int, ?before:GpuEncoder->Void):haxe.io.Bytes {
        var readback = device.createBuffer(new GpuBufferDescriptor(size,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));
        var encoder = device.encoder();
        if (before != null) before(encoder);
        encoder.computeBegin();
        encoder.computeSetPipeline(pipeline);
        encoder.computeSetBindGroup(0, group);
        encoder.computeDispatch(1, 1, 1);
        encoder.computeEnd();
        encoder.copyBuffer(out, 0, readback, 0, size);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        var error = device.takeError();
        check(error == null, error);
        device.mapBuffer(readback, 0, size).await();
        var bytes = haxe.io.Bytes.alloc(size);
        check(readback.copyOut(0, bytes, size), "readback failed");
        readback.unmap();
        readback.destroy();
        return bytes;
    }

    /** A 1x1 rgba8unorm texture holding one texel. */
    static function texel(device:GpuDevice, queue:GpuQueue, r:Int, g:Int, b:Int, a:Int):GpuTexture {
        var texture = device.texture(new GpuTextureDescriptor(new GpuExtent3D(1), Rgba8unorm,
            TextureUsage.TEXTURE_BINDING() | TextureUsage.COPY_DST()));
        var bytes = haxe.io.Bytes.alloc(4);
        bytes.set(0, r);
        bytes.set(1, g);
        bytes.set(2, b);
        bytes.set(3, a);
        queue.writeTexture(texture, bytes, 1, 1, 4);
        return texture;
    }

    static function storageOut(device:GpuDevice, size:Int):GpuBuffer {
        return device.createBuffer(new GpuBufferDescriptor(size, BufferUsage.STORAGE() | BufferUsage.COPY_SRC()));
    }

    /** Arrays of textures and of storage buffers, each two long, read by constant index. */
    static function bindingArrays(device:GpuDevice, queue:GpuQueue) {
        var first = texel(device, queue, 10, 0, 0, 255);
        var second = texel(device, queue, 20, 0, 0, 255);
        var inputs = [for (value in [7, 9]) {
            var buffer = device.createBuffer(new GpuBufferDescriptor(4, BufferUsage.STORAGE() | BufferUsage.COPY_DST()));
            var bytes = haxe.io.Bytes.alloc(4);
            bytes.setInt32(0, value);
            queue.writeBuffer(buffer, 0, bytes, 4);
            buffer;
        }];
        var out = storageOut(device, 16);

        var textures = new GpuBindGroupLayoutEntry(0, ShaderStage.COMPUTE());
        textures.texture(new GpuTextureBindingLayout());
        textures.count(2);
        var readOnly = new GpuBufferBindingLayout();
        readOnly.type(ReadOnlyStorage);
        var buffers = new GpuBindGroupLayoutEntry(1, ShaderStage.COMPUTE());
        buffers.buffer(readOnly);
        buffers.count(2);
        var writable = new GpuBufferBindingLayout();
        writable.type(Storage);
        var output = new GpuBindGroupLayoutEntry(2, ShaderStage.COMPUTE());
        output.buffer(writable);
        var layoutDescriptor = new GpuBindGroupLayoutDescriptor();
        layoutDescriptor.addEntries(textures);
        layoutDescriptor.addEntries(buffers);
        layoutDescriptor.addEntries(output);
        var layout = device.createBindGroupLayout(layoutDescriptor);
        var pipelineLayout = new GpuPipelineLayoutDescriptor();
        pipelineLayout.addBindGroupLayouts(layout);

        var shader = device.createShader('
            enable wgpu_binding_array;
            struct Value { v: u32 };
            @group(0) @binding(0) var textures: binding_array<texture_2d<f32>, 2>;
            @group(0) @binding(1) var<storage, read> inputs: binding_array<Value, 2>;
            @group(0) @binding(2) var<storage, read_write> out: array<u32, 4>;
            @compute @workgroup_size(1) fn main() {
                out[0] = u32(textureLoad(textures[0], vec2<i32>(0, 0), 0).r * 255.0 + 0.5);
                out[1] = u32(textureLoad(textures[1], vec2<i32>(0, 0), 0).r * 255.0 + 0.5);
                out[2] = inputs[0].v;
                out[3] = inputs[1].v;
            }');
        var descriptor = new GpuComputePipelineDescriptor(new GpuProgrammableStage(shader));
        descriptor.layout(device.createPipelineLayout(pipelineLayout));
        var pipeline = device.createComputePipeline(descriptor);

        var views = new GpuTextureViewArray();
        views.addViews(first.createView(new GpuTextureViewDescriptor()));
        views.addViews(second.createView(new GpuTextureViewDescriptor()));
        var ranges = new GpuBufferArray();
        for (buffer in inputs) ranges.addBuffers(new GpuBufferBinding(buffer));
        var viewsEntry = new GpuBindGroupEntry(0);
        viewsEntry.resourceTextureViewArray(views);
        var rangesEntry = new GpuBindGroupEntry(1);
        rangesEntry.resourceBufferArray(ranges);
        var outEntry = new GpuBindGroupEntry(2);
        outEntry.resourceBuffer(out);
        var groupDescriptor = new GpuBindGroupDescriptor(layout);
        groupDescriptor.addEntries(viewsEntry);
        groupDescriptor.addEntries(rangesEntry);
        groupDescriptor.addEntries(outEntry);
        var group = device.createBindGroup(groupDescriptor);

        var result = dispatchOnce(device, queue, pipeline, group, out, 16);
        check(result.getInt32(0) == 10 && result.getInt32(4) == 20, "texture array read the wrong texels");
        check(result.getInt32(8) == 7 && result.getInt32(12) == 9, "buffer array read the wrong values");
    }

    /** One RGBA plane sampled through texture_external. */
    static function externalTexture(device:GpuDevice, queue:GpuQueue) {
        var plane = texel(device, queue, 40, 80, 120, 255);
        var descriptor = new GpuExternalTextureDescriptor(Rgba);
        descriptor.addPlanes(plane.createView(new GpuTextureViewDescriptor()));
        var video = device.createExternalTexture(descriptor);
        check(video.valid(), "external texture was not created");
        var twoPlanes = new GpuExternalTextureDescriptor(Rgba);
        twoPlanes.addPlanes(plane.createView(new GpuTextureViewDescriptor()));
        twoPlanes.addPlanes(plane.createView(new GpuTextureViewDescriptor()));
        refused("an RGBA external texture took two planes", () -> device.createExternalTexture(twoPlanes));

        var shader = device.createShader('
            @group(0) @binding(0) var video: texture_external;
            @group(0) @binding(1) var<storage, read_write> out: array<u32, 4>;
            @compute @workgroup_size(1) fn main() {
                let texel = textureLoad(video, vec2<u32>(0u, 0u));
                out[0] = u32(texel.r * 255.0 + 0.5);
                out[1] = u32(texel.g * 255.0 + 0.5);
                out[2] = u32(texel.b * 255.0 + 0.5);
                out[3] = u32(texel.a * 255.0 + 0.5);
            }');
        var pipeline = device.createComputePipeline(new GpuComputePipelineDescriptor(new GpuProgrammableStage(shader)));
        var out = storageOut(device, 16);
        var videoEntry = new GpuBindGroupEntry(0);
        videoEntry.resourceExternalTexture(video);
        var outEntry = new GpuBindGroupEntry(1);
        outEntry.resourceBuffer(out);
        var groupDescriptor = new GpuBindGroupDescriptor(pipeline.getBindGroupLayout(0));
        groupDescriptor.addEntries(videoEntry);
        groupDescriptor.addEntries(outEntry);
        var result = dispatchOnce(device, queue, pipeline, device.createBindGroup(groupDescriptor), out, 16);
        var rgba = [for (i in 0...4) result.getInt32(i * 4)];
        check(rgba.join(",") == "40,80,120,255", 'external texture read ${rgba.join(",")}');
    }

    /**
        One triangle in a BLAS, placed one unit along z by its TLAS instance.
        A ray down z hits it at t = 3 and reports the instance's custom data;
        a ray beside it misses. The BLAS is then compacted and the scene
        rebuilt from the compacted copy.
    **/
    static function rayQuery(device:GpuDevice, queue:GpuQueue) {
        var corners = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        var bytes = haxe.io.Bytes.alloc(corners.length * 4);
        for (i in 0...corners.length) bytes.setFloat(i * 4, corners[i]);
        var vertices = device.createBuffer(new GpuBufferDescriptor(bytes.length,
            BufferUsage.BLAS_INPUT() | BufferUsage.COPY_DST()));
        queue.writeBuffer(vertices, 0, bytes, bytes.length);

        // Opaque, so a hit commits without the shader confirming it.
        var triangle = new GpuBlasTriangleGeometrySize(Float32x3, 3);
        triangle.flags(AccelerationStructureGeometryFlag.OPAQUE());
        var blasDescriptor = new GpuBlasDescriptor();
        blasDescriptor.flags(AccelerationStructureFlag.PREFER_FAST_TRACE() | AccelerationStructureFlag.ALLOW_COMPACTION());
        blasDescriptor.addTriangles(triangle);
        var blas = device.createBlas(blasDescriptor);
        var tlas = device.createTlas(new GpuTlasDescriptor(1));
        check(tlas.maxInstances() == 1, "TLAS capacity");

        function place(blas) {
            var instance = new GpuTlasInstance(blas);
            for (value in [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0]) instance.addTransform(value);
            instance.customData(42);
            tlas.setInstance(0, instance);
        }
        place(blas);
        var wide = new GpuTlasInstance(blas);
        wide.customData(1 << 24);
        refused("custom data past 24 bits was accepted", () -> tlas.setInstance(0, wide));
        refused("an instance past the TLAS was accepted", () -> tlas.setInstance(1, new GpuTlasInstance(blas)));

        var shader = device.createShader('
            enable wgpu_ray_query;
            @group(0) @binding(0) var scene: acceleration_structure;
            @group(0) @binding(1) var<storage, read_write> out: array<f32, 4>;
            fn shoot(origin: vec3<f32>) -> RayIntersection {
                var query: ray_query;
                rayQueryInitialize(&query, scene, RayDesc(0u, 0xFFu, 0.0, 100.0, origin, vec3<f32>(0.0, 0.0, 1.0)));
                rayQueryProceed(&query);
                return rayQueryGetCommittedIntersection(&query);
            }
            @compute @workgroup_size(1) fn main() {
                let hit = shoot(vec3<f32>(0.25, 0.25, -2.0));
                out[0] = f32(hit.kind);
                out[1] = hit.t;
                out[2] = f32(hit.instance_custom_data);
                out[3] = f32(shoot(vec3<f32>(5.0, 5.0, -2.0)).kind);
            }');
        var pipeline = device.createComputePipeline(new GpuComputePipelineDescriptor(new GpuProgrammableStage(shader)));
        var out = storageOut(device, 16);
        var sceneEntry = new GpuBindGroupEntry(0);
        sceneEntry.resourceAccelerationStructure(tlas);
        var outEntry = new GpuBindGroupEntry(1);
        outEntry.resourceBuffer(out);
        var groupDescriptor = new GpuBindGroupDescriptor(pipeline.getBindGroupLayout(0));
        groupDescriptor.addEntries(sceneEntry);
        groupDescriptor.addEntries(outEntry);
        var group = device.createBindGroup(groupDescriptor);

        function traceScene(build:GpuAccelerationStructureBuild, what:String) {
            var result = dispatchOnce(device, queue, pipeline, group, out, 16,
                encoder -> encoder.buildAccelerationStructures(build));
            check(result.getFloat(0) == 1, '$what: the ray missed the triangle');
            check(Math.abs(result.getFloat(4) - 3) < 1e-4, '$what: hit at t = ${result.getFloat(4)}');
            check(result.getFloat(8) == 42, '$what: custom data ${result.getFloat(8)}');
            check(result.getFloat(12) == 0, '$what: the ray beside the triangle hit');
        }
        var entry = new GpuBlasBuildEntry(blas);
        entry.addTriangles(new GpuBlasTriangleGeometry(triangle, vertices, 12));
        var build = new GpuAccelerationStructureBuild();
        build.addBlases(entry);
        build.addTlases(tlas);
        traceScene(build, "built");

        blas.prepareCompaction().await();
        check(blas.readyForCompaction(), "the BLAS is not ready to compact");
        var compacted = queue.compactBlas(blas);
        check(compacted.valid(), "compaction made no BLAS");
        place(compacted);
        blas.destroy();
        var rebuild = new GpuAccelerationStructureBuild();
        rebuild.addTlases(tlas);
        traceScene(rebuild, "compacted");
    }

    /** A task shader hands a colour to a mesh shader that covers the target. */
    static function meshShader(device:GpuDevice, queue:GpuQueue) {
        var shader = device.createShader('
            enable wgpu_mesh_shader;
            struct Payload { green: f32 };
            var<task_payload> payload: Payload;
            @task @payload(payload) @workgroup_size(1)
            fn ts() -> @builtin(mesh_task_size) vec3<u32> {
                payload.green = 1.0;
                return vec3<u32>(1u, 1u, 1u);
            }
            struct Vertex { @builtin(position) position: vec4<f32>, @location(0) green: f32 };
            struct Primitive { @builtin(triangle_indices) indices: vec3<u32> };
            struct Mesh {
                @builtin(vertices) vertices: array<Vertex, 3>,
                @builtin(primitives) primitives: array<Primitive, 1>,
                @builtin(vertex_count) vertex_count: u32,
                @builtin(primitive_count) primitive_count: u32,
            };
            var<workgroup> mesh: Mesh;
            @mesh(mesh) @payload(payload) @workgroup_size(1)
            fn ms() {
                mesh.vertex_count = 3u;
                mesh.primitive_count = 1u;
                mesh.vertices[0] = Vertex(vec4<f32>(-1.0, -1.0, 0.0, 1.0), payload.green);
                mesh.vertices[1] = Vertex(vec4<f32>(3.0, -1.0, 0.0, 1.0), payload.green);
                mesh.vertices[2] = Vertex(vec4<f32>(-1.0, 3.0, 0.0, 1.0), payload.green);
                mesh.primitives[0].indices = vec3<u32>(0u, 1u, 2u);
            }
            @fragment fn fs(v: Vertex) -> @location(0) vec4<f32> {
                return vec4<f32>(0.0, v.green, 0.0, 1.0);
            }');
        var mesh = new GpuProgrammableStage(shader);
        mesh.entryPoint("ms");
        var task = new GpuProgrammableStage(shader);
        task.entryPoint("ts");
        var fragment = new GpuFragmentState(shader);
        fragment.entryPoint("fs");
        fragment.addTargets(new GpuColorTargetState(Rgba8unorm));
        var descriptor = new GpuMeshPipelineDescriptor(mesh);
        descriptor.task(task);
        descriptor.fragment(fragment);
        var pipeline = device.createMeshPipelineAsync(descriptor).await();
        check(pipeline.valid(), "mesh pipeline failed");

        var size = new GpuExtent3D(4);
        size.height(4);
        var target = device.texture(new GpuTextureDescriptor(size, Rgba8unorm,
            TextureUsage.RENDER_ATTACHMENT() | TextureUsage.COPY_SRC()));
        var pixels = device.createBuffer(new GpuBufferDescriptor(256 * 4,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));
        var colour = new GpuRenderPassColorAttachment(Clear, Store);
        colour.viewTexture(target);
        colour.clearValue(new GpuColor(0, 0, 1, 1));
        var pass = new GpuRenderPassDescriptor();
        pass.addColorAttachments(colour);
        var encoder = device.encoder();
        encoder.beginRenderPass(pass);
        encoder.renderSetPipeline(pipeline);
        encoder.renderDrawMeshTasks(1, 1, 1);
        encoder.renderEnd();
        var into = new GpuTexelCopyBufferInfo(pixels);
        into.bytesPerRow(256);
        encoder.copyTextureToBufferWith(new GpuTexelCopyTextureInfo(target), into, size);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBuffer(pixels, 0, 256 * 4).await();
        var image = haxe.io.Bytes.alloc(256 * 4);
        check(pixels.copyOut(0, image, image.length), "mesh readback failed");
        for (y in 0...4) for (x in 0...4) {
            var at = y * 256 + x * 4;
            check(image.get(at) == 0 && image.get(at + 1) == 255 && image.get(at + 2) == 0,
                'mesh pixel ($x, $y) is ${image.get(at)},${image.get(at + 1)},${image.get(at + 2)}');
        }
        pixels.unmap();
    }

    /**
        wgpu's own extensions where the adapter has them, on a device that
        asks for them and for every native limit the adapter reports:
        binding arrays, ray queries and mesh shaders all default to none.
        Ray queries and mesh shaders are experimental, which the device
        descriptor has to accept.
    **/
    static function wgpuExtensions(adapter:GpuAdapter) {
        var requested = new GpuDeviceDescriptor();
        for (feature in [TextureBindingArray, BufferBindingArray, StorageResourceBindingArray, ExternalTexture,
                ExperimentalRayQuery, ExperimentalMeshShader])
            if (adapter.supportsNative(feature)) requested.addRequiredNativeFeatures(feature);
        for (limit in Type.allEnums(NativeLimit)) requested.addRequiredNativeLimits(limit, adapter.nativeLimit(limit));
        if (adapter.supportsNative(ExperimentalRayQuery) || adapter.supportsNative(ExperimentalMeshShader))
            refused("an experimental feature was granted without the opt-in",
                () -> adapter.requestDeviceWith(requested).await());
        requested.experimentalFeatures(true);
        var device = adapter.requestDeviceWith(requested).await();
        var queue = device.queue();
        var ran = [];
        if (device.supportsNative(TextureBindingArray) && device.supportsNative(BufferBindingArray)
                && device.supportsNative(StorageResourceBindingArray)) {
            bindingArrays(device, queue);
            ran.push("binding arrays");
        }
        if (device.supportsNative(ExternalTexture)) {
            externalTexture(device, queue);
            ran.push("external texture");
        }
        if (device.supportsNative(ExperimentalRayQuery)) {
            rayQuery(device, queue);
            ran.push("ray query");
        }
        if (device.supportsNative(ExperimentalMeshShader)) {
            meshShader(device, queue);
            ran.push("mesh shader");
        }
        check(device.takeError() == null, "GPU validation error in wgpu extensions");
        device.destroy();
        Sys.println('gpu wgpu extensions ok: ${ran.join(", ")}');
    }

    /**
        What adapters, buffers and textures report, writes through mapped
        buffers, and an instance and adapter asked for with options.
    **/
    static function introspection(adapter:GpuAdapter) {
        var min = adapter.subgroupMinSize();
        var max = adapter.subgroupMaxSize();
        check(min > 0 && min <= max, 'subgroup sizes $min..$max');
        Sys.println('adapter: ${adapter.deviceType()}, vendor ${adapter.vendorId()}, subgroups $min..$max');

        var configured = new GpuInstanceDescriptor();
        configured.backends(Backends.VULKAN() | Backends.METAL() | Backends.DX12() | Backends.GL());
        configured.flags(InstanceFlag.VALIDATION());
        var instance = GpuInstance.createWith(configured);
        var options = new GpuRequestAdapterOptions();
        options.powerPreference(HighPerformance);
        var chosen = instance.requestAdapterWith(options).await();
        check(chosen.valid(), "no adapter for the configured instance");
        chosen.destroy();
        instance.destroy();

        var requested = new GpuDeviceDescriptor();
        requested.memoryHints(MemoryUsage);
        var device = adapter.requestDeviceWith(requested).await();
        var queue = device.queue();

        var data = haxe.io.Bytes.alloc(16);
        for (i in 0...4) data.setInt32(i * 4, (i + 1) * 11);
        var writable = device.createBuffer(new GpuBufferDescriptor(16, BufferUsage.MAP_WRITE() | BufferUsage.COPY_SRC()));
        check(writable.size() == 16, "buffer size");
        check(writable.usage() == (BufferUsage.MAP_WRITE() | BufferUsage.COPY_SRC()), "buffer usage");
        check(!writable.copyIn(0, data, 16), "an unmapped buffer took a write");
        device.mapBufferWith(writable, MapMode.WRITE(), 0, 16).await();
        check(writable.copyIn(0, data, 16), "the write mapping refused the data");
        writable.unmap();
        var atCreation = new GpuBufferDescriptor(16, BufferUsage.COPY_SRC());
        atCreation.mappedAtCreation(true);
        var created = device.createBuffer(atCreation);
        check(created.copyIn(0, data, 8), "a buffer mapped at creation refused the data");
        created.unmap();
        var readback = device.createBuffer(new GpuBufferDescriptor(32, BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));
        var encoder = device.encoder();
        encoder.copyBuffer(writable, 0, readback, 0, 16);
        encoder.copyBuffer(created, 0, readback, 16, 16);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBufferWith(readback, MapMode.READ(), 0, 32).await();
        var out = haxe.io.Bytes.alloc(32);
        check(readback.copyOut(0, out, 32), "mapped readback failed");
        for (i in 0...4) check(out.getInt32(i * 4) == (i + 1) * 11, "the mapped write did not land");
        check(out.getInt32(16) == 11 && out.getInt32(20) == 22 && out.getInt32(24) == 0, "the write at creation did not land");
        readback.unmap();
        refused("a map mode of 3 was accepted", () -> device.mapBufferWith(readback, 3, 0, 32).await());

        var size = new GpuExtent3D(8);
        size.height(4);
        size.depthOrArrayLayers(2);
        var described = new GpuTextureDescriptor(size, Rgba16float, TextureUsage.TEXTURE_BINDING() | TextureUsage.COPY_DST());
        described.mipLevelCount(2);
        var texture = device.texture(described);
        check(texture.width() == 8 && texture.height() == 4 && texture.depthOrArrayLayers() == 2, "texture size");
        check(texture.mipLevelCount() == 2 && texture.sampleCount() == 1, "texture levels");
        check(Type.enumEq(texture.dimension(), TextureDimension.D2d), 'texture dimension ${texture.dimension()}');
        check(Type.enumEq(texture.format(), Rgba16float), 'texture format ${texture.format()}');
        check(texture.usage() == (TextureUsage.TEXTURE_BINDING() | TextureUsage.COPY_DST()), "texture usage");

        check(device.takeError() == null, "GPU validation error in introspection");
        device.destroy();
        Sys.println("gpu introspection ok");
    }

    /**
        A DontCare load on a device that accepted it: the pass writes every
        pixel before it stores, so the target is defined.
    **/
    static function dontCareLoads(adapter:GpuAdapter) {
        var requested = new GpuDeviceDescriptor();
        requested.dontCareLoads(true);
        var device = adapter.requestDeviceWith(requested).await();
        var queue = device.queue();
        var shader = device.createShader('
            @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                let x = f32(i32(i) / 2) * 4.0 - 1.0;
                let y = f32(i32(i) % 2) * 4.0 - 1.0;
                return vec4<f32>(x, y, 0.0, 1.0);
            }
            @fragment fn fs() -> @location(0) vec4<f32> {
                return vec4<f32>(1.0, 0.0, 1.0, 1.0);
            }');
        var vertex = new GpuVertexState(shader);
        vertex.entryPoint("vs");
        var fragment = new GpuFragmentState(shader);
        fragment.entryPoint("fs");
        fragment.addTargets(new GpuColorTargetState(Rgba8unorm));
        var descriptor = new GpuRenderPipelineDescriptor(vertex);
        descriptor.fragment(fragment);
        var pipeline = device.createRenderPipeline(descriptor);

        var size = new GpuExtent3D(4);
        size.height(4);
        var target = device.texture(new GpuTextureDescriptor(size, Rgba8unorm,
            TextureUsage.RENDER_ATTACHMENT() | TextureUsage.COPY_SRC()));
        var pixels = device.createBuffer(new GpuBufferDescriptor(256 * 4,
            BufferUsage.MAP_READ() | BufferUsage.COPY_DST()));
        var colour = new GpuRenderPassColorAttachment(DontCare, Store);
        colour.viewTexture(target);
        var pass = new GpuRenderPassDescriptor();
        pass.addColorAttachments(colour);
        var encoder = device.encoder();
        encoder.beginRenderPass(pass);
        encoder.renderSetPipeline(pipeline);
        encoder.renderDrawRange(3, 1, 0, 0);
        encoder.renderEnd();
        var into = new GpuTexelCopyBufferInfo(pixels);
        into.bytesPerRow(256);
        encoder.copyTextureToBufferWith(new GpuTexelCopyTextureInfo(target), into, size);
        encoder.submit(queue);
        device.queueWorkDone(queue).await();
        device.mapBuffer(pixels, 0, 256 * 4).await();
        var image = haxe.io.Bytes.alloc(256 * 4);
        check(pixels.copyOut(0, image, image.length), "DontCare readback failed");
        for (y in 0...4) for (x in 0...4) {
            var at = y * 256 + x * 4;
            check(image.get(at) == 255 && image.get(at + 1) == 0 && image.get(at + 2) == 255,
                'DontCare pixel ($x, $y) was not written');
        }
        pixels.unmap();
        check(device.takeError() == null, "GPU validation error with a DontCare load");
        device.destroy();
        Sys.println("gpu DontCare load ok");
    }

    static final TRIPLE = '
        @group(0) @binding(0) var<storage, read_write> values: array<u32, 4>;
        @compute @workgroup_size(4) fn main(@builtin(local_invocation_index) i: u32) {
            values[i] = values[i] * 3u;
        }';

    /** Runs `pipeline` over four integers in a storage buffer and checks each tripled. */
    static function tripled(device:GpuDevice, queue:GpuQueue, pipeline:GpuPipeline, layout:gpu.GpuBindGroupLayout,
            what:String) {
        var values = device.createBuffer(new GpuBufferDescriptor(16,
            BufferUsage.STORAGE() | BufferUsage.COPY_DST() | BufferUsage.COPY_SRC()));
        var data = haxe.io.Bytes.alloc(16);
        for (i in 0...4) data.setInt32(i * 4, i + 1);
        queue.writeBuffer(values, 0, data, 16);
        var entry = new GpuBindGroupEntry(0);
        entry.resourceBuffer(values);
        var group = new GpuBindGroupDescriptor(layout);
        group.addEntries(entry);
        var out = dispatchOnce(device, queue, pipeline, device.createBindGroup(group), values, 16);
        for (i in 0...4) check(out.getInt32(i * 4) == (i + 1) * 3, '$what: value $i was not tripled');
    }

    /**
        Shaders wgpu does not check, on a device that vouches for them: WGSL
        with every runtime check off, and on Metal an MSL kernel handed to
        the backend as it is.
    **/
    static function trustedShaders(adapter:GpuAdapter, device:GpuDevice) {
        var checked = device.createShaderModule(new GpuShaderModuleDescriptor(TRIPLE));
        check(checked.valid(), "a checked shader module was refused");
        var unchecked = new GpuShaderModuleDescriptor(TRIPLE);
        unchecked.boundsChecks(false);
        refused("a shader without bounds checks was accepted untrusted", () -> device.createShaderModule(unchecked));
        var msl = new GpuPassthroughShaderDescriptor();
        msl.msl("kernel void triple() {}");
        refused("a passthrough shader was accepted untrusted", () -> device.createShaderPassthrough(msl));

        var requested = new GpuDeviceDescriptor();
        requested.trustedShaders(true);
        var passthrough = adapter.supportsNative(PassthroughShaders) && Type.enumEq(adapter.backend(), Metal);
        if (passthrough) requested.addRequiredNativeFeatures(PassthroughShaders);
        var trusted = adapter.requestDeviceWith(requested).await();
        var queue = trusted.queue();

        var fast = new GpuShaderModuleDescriptor(TRIPLE);
        for (off in [fast.boundsChecks, fast.forceLoopBounding, fast.rayQueryInitializationTracking,
                fast.taskShaderDispatchTracking, fast.meshShaderPrimitiveIndicesClamp, fast.intDivChecks])
            off(false);
        var pipeline = trusted.createComputePipeline(new GpuComputePipelineDescriptor(
            new GpuProgrammableStage(trusted.createShaderModule(fast))));
        tripled(trusted, queue, pipeline, pipeline.getBindGroupLayout(0), "unchecked WGSL");

        if (passthrough) {
            // Metal numbers buffers in layout order, so binding 0 is buffer 0.
            var source = new GpuPassthroughShaderDescriptor();
            source.msl('
                #include <metal_stdlib>
                using namespace metal;
                kernel void triple(device uint* values [[buffer(0)]], uint i [[thread_position_in_grid]]) {
                    values[i] = values[i] * 3u;
                }');
            var point = new GpuPassthroughEntryPoint("triple");
            point.workgroupX(4);
            source.addEntryPoints(point);
            var writable = new GpuBufferBindingLayout();
            writable.type(Storage);
            var binding = new GpuBindGroupLayoutEntry(0, ShaderStage.COMPUTE());
            binding.buffer(writable);
            var layoutDescriptor = new GpuBindGroupLayoutDescriptor();
            layoutDescriptor.addEntries(binding);
            var layout = trusted.createBindGroupLayout(layoutDescriptor);
            var pipelineLayout = new GpuPipelineLayoutDescriptor();
            pipelineLayout.addBindGroupLayouts(layout);
            var stage = new GpuProgrammableStage(trusted.createShaderPassthrough(source));
            stage.entryPoint("triple");
            var descriptor = new GpuComputePipelineDescriptor(stage);
            descriptor.layout(trusted.createPipelineLayout(pipelineLayout));
            tripled(trusted, queue, trusted.createComputePipeline(descriptor), layout, "passthrough MSL");
        }
        check(trusted.takeError() == null, "GPU validation error with trusted shaders");
        trusted.destroy();
        Sys.println('gpu trusted shaders ok${passthrough ? ": unchecked WGSL, passthrough MSL" : ": unchecked WGSL"}');
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
        wgpuExtensions(adapter);
        dontCareLoads(adapter);
        trustedShaders(adapter, device);
        introspection(adapter);
        device.destroy();
        adapter.destroy();
        instance.destroy();
        Sys.println("gpu compute ok");
    }
}
