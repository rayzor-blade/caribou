import gpu.BufferUsage;
import gpu.ColorWrite;
import gpu.GpuInstance;
import gpu.GpuBufferDescriptor;
import gpu.GpuRequestAdapterOptions;
import gpu.GpuSurfaceConfiguration;
import gpu.PresentMode;
import gpu.TextureUsage;
import gpu.Power;
import gpu.VertexFormat;
import gpu.VertexStepMode;
import window.Event;
import window.WindowBuilder;

/** A triangle rendered through the gpu plugin into a window plugin surface. */
using Lambda;

class Main {
    static final SHADER = '
        @vertex
        fn vs(@location(0) position: vec2<f32>) -> @builtin(position) vec4<f32> {
            return vec4<f32>(position, 0.0, 1.0);
        }

        @fragment
        fn fs() -> @location(0) vec4<f32> {
            return vec4<f32>(0.95, 0.45, 0.1, 1.0);
        }
    ';

    static function check(ok:Bool, message:String) {
        if (!ok) throw message;
    }

    static function main() {
        // An optional frame limit makes the interactive example scriptable:
        // `cargo run -p caribou-plugin-fixtures --bin window_gpu -- 3`.
        var frameLimit = Sys.args().length == 0 ? -1 : Std.parseInt(Sys.args()[0]);
        if (frameLimit == null) frameLimit = -1;

        var window = new WindowBuilder()
            .title("Caribou GPU Triangle")
            .size(800, 500)
            .resizable(true)
            .open();
        check(window.width() > 0 && window.height() > 0, "window creation failed");

        var instance = new GpuInstance();
        // Raw handles are plain integers because the two plugins share no
        // Rust types. The window remains alive until after surface.destroy().
        var surface = instance.surface(
            window.platform(),
            window.raw(0), window.raw(1),
            window.raw(2), window.raw(3)
        );
        check(surface.valid(), "this window cannot create a GPU surface");

        // An adapter that can present to this surface.
        var options = new GpuRequestAdapterOptions();
        options.powerPreference(HighPerformance);
        options.compatibleSurface(surface);
        var adapter = instance.requestAdapterWith(options).await();
        check(adapter.valid(), "no GPU adapter is available");
        var device = adapter.requestDevice().await();
        check(device.valid(), "GPU device creation failed");
        var queue = device.queue();
        var format = surface.preferredFormat(adapter);

        // What this surface supports on this adapter; Fifo is always there.
        var capabilities = surface.capabilities(adapter);
        var formats = [for (i in 0...capabilities.formatCount()) capabilities.format(i)];
        check(formats.exists(f -> Type.enumEq(f, format)), "the preferred format is not supported");
        var modes = [for (i in 0...capabilities.presentModeCount()) capabilities.presentMode(i)];
        check(modes.exists(m -> Type.enumEq(m, PresentMode.Fifo)), "the surface lacks Fifo presentation");
        check(capabilities.alphaModeCount() > 0, "the surface reports no alpha mode");
        check((capabilities.usages() & TextureUsage.RENDER_ATTACHMENT()) != 0, "the surface cannot be rendered to");
        var alpha = capabilities.alphaMode(0);

        function configure() {
            var configuration = new GpuSurfaceConfiguration(format, window.width(), window.height());
            configuration.presentMode(Fifo);
            configuration.alphaMode(alpha);
            configuration.desiredMaximumFrameLatency(2);
            device.configureSurfaceWith(surface, configuration);
        }
        configure();

        var corners = [-0.8, -0.65, 0.8, -0.65, 0.0, 0.8];
        var vertexData = haxe.io.Bytes.alloc(corners.length * 4);
        for (i in 0...corners.length) vertexData.setFloat(i * 4, corners[i]);
        var vertices = device.createBuffer(new GpuBufferDescriptor(
            vertexData.length,
            BufferUsage.VERTEX() | BufferUsage.COPY_DST()
        ));
        queue.writeBuffer(vertices, 0, vertexData, vertexData.length);

        var shader = device.createShader(SHADER);
        var pipelineBuilder = device.pipeline();
        pipelineBuilder.shader(shader, "vs", "fs");
        pipelineBuilder.vertexBuffer(8, VertexStepMode.Vertex);
        pipelineBuilder.attribute(VertexFormat.Float32x2, 0, 0);
        pipelineBuilder.target(format, ColorWrite.ALL());
        var pipeline = pipelineBuilder.build();
        check(pipeline.valid(), "render pipeline creation failed");

        Sys.println('GPU: ${adapter.name()} (${adapter.backend()})');
        var frames = 0;
        var running = true;
        function render() {
            var view = surface.acquire();
            if (!view.valid()) {
                configure();
                return;
            }
            var encoder = device.encoder();
            encoder.passColour(view, 0.06, 0.07, 0.09, 1.0);
            encoder.passBegin();
            encoder.renderSetPipeline(pipeline);
            encoder.renderSetVertexBuffer(0, vertices);
            encoder.renderDraw(3, 1);
            encoder.renderEnd();
            encoder.submit(queue);
            queue.presentSurface(surface);
            frames++;
        }
        window.request_redraw();
        while (running && (frameLimit < 0 || frames < frameLimit)) {
            switch (window.poll()) {
                case Closed | Destroyed:
                    running = false;
                case Resized(width, height):
                    if (width.low > 0 && height.low > 0) configure();
                    window.request_redraw();
                case RedrawRequested:
                    render();
                    window.request_redraw();
                case None:
                    Sys.sleep(0.001);
                default:
            }
        }

        pipeline.destroy();
        shader.destroy();
        vertices.destroy();
        surface.destroy();
        device.destroy();
        adapter.destroy();
        instance.destroy();
        window.close();
        Sys.println('triangle rendered $frames frame(s)');
    }
}
