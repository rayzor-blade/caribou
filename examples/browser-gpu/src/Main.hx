import gpu.BufferUsage;
import gpu.ColorWrite;
import gpu.GpuBindGroupDescriptor;
import gpu.GpuBindGroupEntry;
import gpu.GpuBufferDescriptor;
import gpu.GpuComputePipelineDescriptor;
import gpu.GpuInstance;
import gpu.GpuProgrammableStage;
import gpu.VertexFormat;
import gpu.VertexStepMode;
import window.Window;
import window.WindowAttributes;

/** Moves each particle, stored as position then velocity. **/
class Step implements caribou.hxsl.Shader {
	static var SRC = {
		@param var particles : RWBuffer<Vec4, 4096>;
		@param var dt : Float;
		@param var time : Float;
		function main() {
			setLayout(64);
			var i = computeVar.globalInvocation.x;
			var p = particles[i];
			// Around a centre that wanders, faster near it.
			var centre = vec2(cos(time * 0.7), sin(time * 1.1)) * 0.4;
			var d = p.xy - centre;
			var swirl = vec2(-d.y, d.x) / (0.05 + dot(d, d));
			var velocity = normalize(mix(p.zw, swirl, 0.04)) * clamp(length(swirl) * 0.25, 0.1, 0.9);
			var position = p.xy + velocity * dt;
			if (abs(position.x) > 1.0) {
				velocity.x = -velocity.x;
				position.x = clamp(position.x, -1.0, 1.0);
			}
			if (abs(position.y) > 1.0) {
				velocity.y = -velocity.y;
				position.y = clamp(position.y, -1.0, 1.0);
			}
			particles[i] = vec4(position, velocity);
		}
	};
}

/** Each particle as a small arrow pointing where it is heading. **/
class Arrow implements caribou.hxsl.Shader {
	static var SRC = {
		@input var input : { position : Vec2, velocity : Vec2 };
		var output : { position : Vec4, color : Vec4 };
		@param var time : Float;
		@param var aspect : Float;
		@param var count : Float;
		var colour : Vec3;
		function vertex() {
			var corner = vec2(-0.012, 0.008);
			if (vertexID == 0) corner = vec2(0.018, 0.0);
			if (vertexID == 2) corner = vec2(-0.012, -0.008);
			var a = atan(input.velocity.y, input.velocity.x);
			var turned = vec2(corner.x * cos(a) - corner.y * sin(a), corner.x * sin(a) + corner.y * cos(a));
			output.position = vec4(input.position + turned * vec2(aspect, 1.0), 0.0, 1.0);
			var t = float(instanceID) / count + time * 0.05;
			colour = cos((vec3(0.0, 0.33, 0.67) + t) * 6.2831) * 0.45 + 0.55;
		}
		function fragment() {
			output.color = vec4(colour, 1.0);
		}
	};
}

/**
	A swarm on the GPU, drawn in a window: each frame a compute pass moves
	the particles, and a render pass draws each one as a small arrow pointing
	where it is heading. Both shaders are HXSL. On a desktop the window is a
	native one; in a page it is the page's canvas.
**/
class Main {
	static inline final COUNT = 4096;

	static function main() {
		var attributes = new WindowAttributes();
		attributes.title("Swarm");
		attributes.width(800);
		attributes.height(500);
		var window = Window.open(attributes);
		var instance = new GpuInstance();
		var surface = instance.surface(window.platform(), window.raw(0), window.raw(1), window.raw(2), window.raw(3));
		var adapter = instance.requestAdapter(HighPerformance).await();
		var device = adapter.requestDevice().await();
		var queue = device.queue();
		var format = surface.preferredFormat(adapter);
		trace('drawing $COUNT particles as $format');

		// Positions scattered, velocities small and random.
		var initial = haxe.io.Bytes.alloc(COUNT * 16);
		for (i in 0...COUNT) {
			initial.setFloat(i * 16, Math.random() * 2 - 1);
			initial.setFloat(i * 16 + 4, Math.random() * 2 - 1);
			initial.setFloat(i * 16 + 8, Math.random() * 0.2 - 0.1);
			initial.setFloat(i * 16 + 12, Math.random() * 0.2 - 0.1);
		}
		var particles = device.createBuffer(new GpuBufferDescriptor(initial.length,
			BufferUsage.STORAGE() | BufferUsage.VERTEX() | BufferUsage.COPY_DST()));
		queue.writeBuffer(particles, 0, initial, initial.length);

		// Each shader's parameters, laid out as HXSL gives them.
		var stepParams = device.createBuffer(new GpuBufferDescriptor(Step.PARAMS_SIZE,
			BufferUsage.UNIFORM() | BufferUsage.COPY_DST()));
		var arrowParams = device.createBuffer(new GpuBufferDescriptor(Arrow.PARAMS_SIZE,
			BufferUsage.UNIFORM() | BufferUsage.COPY_DST()));

		var stepShader = device.createShader(Step.WGSL);
		var stepping = device.createComputePipeline(new GpuComputePipelineDescriptor(new GpuProgrammableStage(stepShader)));
		var group = new GpuBindGroupDescriptor(stepping.getBindGroupLayout(Step.BUFFER_particles_GROUP));
		var particlesEntry = new GpuBindGroupEntry(Step.BUFFER_particles);
		particlesEntry.resourceBuffer(particles);
		group.addEntries(particlesEntry);
		var stepEntry = new GpuBindGroupEntry(Step.PARAMS_BINDING);
		stepEntry.resourceBuffer(stepParams);
		group.addEntries(stepEntry);
		var stepBound = device.createBindGroup(group);

		// The particles again, read as each arrow's instance data.
		var arrowShader = device.createShader(Arrow.WGSL);
		var builder = device.pipeline();
		builder.shader(arrowShader, "vertex", "fragment");
		builder.vertexBuffer(16, VertexStepMode.Instance);
		builder.attribute(VertexFormat.Float32x2, 0, Arrow.INPUT_position);
		builder.attribute(VertexFormat.Float32x2, 8, Arrow.INPUT_velocity);
		builder.target(format, ColorWrite.ALL());
		var drawing = builder.build();
		var drawGroup = new GpuBindGroupDescriptor(drawing.getBindGroupLayout(Arrow.PARAMS_GROUP));
		var arrowEntry = new GpuBindGroupEntry(Arrow.PARAMS_BINDING);
		arrowEntry.resourceBuffer(arrowParams);
		drawGroup.addEntries(arrowEntry);
		var arrowBound = device.createBindGroup(drawGroup);

		var stepValues = haxe.io.Bytes.alloc(Step.PARAMS_SIZE);
		var arrowValues = haxe.io.Bytes.alloc(Arrow.PARAMS_SIZE);
		arrowValues.setFloat(Arrow.PARAMS_count, COUNT);

		// The surface follows the window, which in a page has a size only
		// once the page has laid it out.
		var configured = false;
		function configure() {
			var width = window.width(), height = window.height();
			if (width <= 0 || height <= 0) return;
			device.configureSurface(surface, width, height, format);
			arrowValues.setFloat(Arrow.PARAMS_aspect, height / width);
			configured = true;
		}
		configure();

		var start = haxe.Timer.stamp(), last = start;
		function render() {
			var now = haxe.Timer.stamp();
			stepValues.setFloat(Step.PARAMS_dt, Math.min(now - last, 0.05));
			stepValues.setFloat(Step.PARAMS_time, now - start);
			arrowValues.setFloat(Arrow.PARAMS_time, now - start);
			last = now;
			queue.writeBuffer(stepParams, 0, stepValues, Step.PARAMS_SIZE);
			queue.writeBuffer(arrowParams, 0, arrowValues, Arrow.PARAMS_SIZE);

			var view = surface.acquire();
			var encoder = device.encoder();
			encoder.computeBegin();
			encoder.computeSetPipeline(stepping);
			encoder.computeSetBindGroup(Step.BUFFER_particles_GROUP, stepBound);
			encoder.computeDispatch(Std.int(COUNT / 64), 1, 1);
			encoder.computeEnd();
			encoder.passColour(view, 0.02, 0.02, 0.05, 1.0);
			encoder.passBegin();
			encoder.renderSetPipeline(drawing);
			encoder.renderSetBindGroup(Arrow.PARAMS_GROUP, arrowBound);
			encoder.renderSetVertexBuffer(0, particles);
			encoder.renderDraw(3, COUNT);
			encoder.renderEnd();
			encoder.submit(queue);
			queue.presentSurface(surface);
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
				case None:
					Sys.sleep(0.001);
				default:
			}
		}
		window.close();
	}
}
