import game.flock.Flock;
import gpu.BufferUsage;
import gpu.ColorWrite;
import gpu.GpuBindGroupDescriptor;
import gpu.GpuBindGroupEntry;
import gpu.GpuBufferDescriptor;
import gpu.GpuInstance;
import gpu.VertexFormat;
import gpu.VertexStepMode;
import window.Window;
import window.WindowAttributes;

/** A bird: a small arrow at its position, turned to its heading. **/
class Bird implements caribou.hxsl.Shader {
	static var SRC = {
		@input var input : { position : Vec2, heading : Float, hue : Float };
		var output : { position : Vec4, color : Vec4 };
		@param var size : Vec2;
		var colour : Vec3;
		function vertex() {
			var corner = vec2(-5.0, 4.0);
			if (vertexID == 0) corner = vec2(8.0, 0.0);
			if (vertexID == 2) corner = vec2(-5.0, -4.0);
			var a = input.heading;
			var turned = vec2(corner.x * cos(a) - corner.y * sin(a), corner.x * sin(a) + corner.y * cos(a));
			var p = input.position + turned;
			output.position = vec4(p.x / size.x * 2.0 - 1.0, 1.0 - p.y / size.y * 2.0, 0.0, 1.0);
			colour = cos((vec3(0.0, 0.33, 0.67) + input.hue) * 6.2831) * 0.45 + 0.55;
		}
		function fragment() {
			output.color = vec4(colour, 1.0);
		}
	};
}

/**
	Haxe as the engine and Wren as the gameplay, in one program: the flock
	lives in `game/flock.wren` (which imports `game/palette.wren`), and this
	engine opens the window, draws the birds on the GPU straight from the
	flock's own `Float32Array`, and hands it the player's clicks. A full flock refuses a click
	with a Wren error, which arrives here as an exception.
**/
class Main {
	static function main() {
		var attributes = new WindowAttributes();
		attributes.title("Flock");
		attributes.width(800);
		attributes.height(500);
		var window = Window.open(attributes);
		var instance = new GpuInstance();
		var surface = instance.surface(window.platform(), window.raw(0), window.raw(1), window.raw(2), window.raw(3));
		var adapter = instance.requestAdapter(HighPerformance).await();
		var device = adapter.requestDevice().await();
		var queue = device.queue();
		var format = surface.preferredFormat(adapter);

		// The gameplay, in Wren.
		var flock = new Flock(200, 800, 500);
		trace('Wren says: ${flock.hud}');

		// Each bird's instance data is the flock's own array, which Haxe
		// reads where it lies and uploads every frame.
		var limit = Std.int(Flock.limit);
		var data = flock.birds;
		var birds = device.createBuffer(new GpuBufferDescriptor(limit * 16, BufferUsage.VERTEX() | BufferUsage.COPY_DST()));
		var params = device.createBuffer(new GpuBufferDescriptor(Bird.PARAMS_SIZE, BufferUsage.UNIFORM() | BufferUsage.COPY_DST()));
		var values = haxe.io.Bytes.alloc(Bird.PARAMS_SIZE);

		var shader = device.createShader(Bird.WGSL);
		var builder = device.pipeline();
		builder.shader(shader, "vertex", "fragment");
		builder.vertexBuffer(16, VertexStepMode.Instance);
		builder.attribute(VertexFormat.Float32x2, 0, Bird.INPUT_position);
		builder.attribute(VertexFormat.Float32, 8, Bird.INPUT_heading);
		builder.attribute(VertexFormat.Float32, 12, Bird.INPUT_hue);
		builder.target(format, ColorWrite.ALL());
		var pipeline = builder.build();
		var group = new GpuBindGroupDescriptor(pipeline.getBindGroupLayout(Bird.PARAMS_GROUP));
		var entry = new GpuBindGroupEntry(Bird.PARAMS_BINDING);
		entry.resourceBuffer(params);
		group.addEntries(entry);
		var bound = device.createBindGroup(group);

		// The surface and the flock's arena follow the window, which in a
		// page has a size only once the page has laid it out.
		var configured = false;
		function configure() {
			var width = window.width(), height = window.height();
			if (width <= 0 || height <= 0) return;
			device.configureSurface(surface, width, height, format);
			values.setFloat(Bird.PARAMS_size, width);
			values.setFloat(Bird.PARAMS_size + 4, height);
			queue.writeBuffer(params, 0, values, Bird.PARAMS_SIZE);
			flock.resize(width, height);
			configured = true;
		}
		configure();

		var last = haxe.Timer.stamp();
		function render() {
			var now = haxe.Timer.stamp();
			flock.step(Math.min(now - last, 0.05));
			last = now;
			var count = Std.int(flock.count);
			queue.writeBuffer(birds, 0, data, count * 16);

			var view = surface.acquire();
			var encoder = device.encoder();
			encoder.passColour(view, 0.03, 0.03, 0.06, 1.0);
			encoder.passBegin();
			encoder.renderSetPipeline(pipeline);
			encoder.renderSetBindGroup(Bird.PARAMS_GROUP, bound);
			encoder.renderSetVertexBuffer(0, birds);
			encoder.renderDraw(3, count);
			encoder.renderEnd();
			encoder.submit(queue);
			queue.presentSurface(surface);
		}

		// A click or a tap spawns birds there; a full flock says no.
		var pointerX = 0.0, pointerY = 0.0;
		function spawn(x:Float, y:Float) {
			try {
				flock.spawn(x, y);
				trace('Wren says: ${flock.hud}');
			} catch (e:Dynamic) {
				trace('Wren refused: $e');
			}
		}

		window.requestRedraw();
		var running = true;
		while (running) {
			switch (window.poll()) {
				case Closed | Destroyed:
					running = false;
				case Resized(_, _):
					configure();
					window.requestRedraw();
				case RedrawRequested:
					window.requestRedraw();
					if (configured) render();
				case CursorMoved(x, y, _):
					pointerX = x;
					pointerY = y;
				case MouseInput(Pressed, Left, _):
					spawn(pointerX, pointerY);
				case Touch(_, Started, x, y, _, _):
					spawn(x, y);
				case None:
					Sys.sleep(0.001);
				default:
			}
		}
		window.close();
	}
}
