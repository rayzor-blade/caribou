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

/**
	A swarm on the GPU, drawn in the page's canvas: each frame a compute
	pass moves the particles, and a render pass draws each one as a small
	arrow pointing where it is heading.
**/
class Main {
	static inline final COUNT = 4096;

	static final SHADER = '
		struct Particle { position: vec2<f32>, velocity: vec2<f32> };
		struct Params { dt: f32, time: f32, aspect: f32, count: f32 };

		@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
		@group(0) @binding(1) var<uniform> params: Params;

		@compute @workgroup_size(64)
		fn step(@builtin(global_invocation_id) id: vec3<u32>) {
			let i = id.x;
			if (i >= arrayLength(&particles)) { return; }
			var p = particles[i];
			// Around a centre that wanders, faster near it.
			let centre = 0.4 * vec2<f32>(cos(params.time * 0.7), sin(params.time * 1.1));
			let d = p.position - centre;
			let swirl = vec2<f32>(-d.y, d.x) / (0.05 + dot(d, d));
			p.velocity = normalize(mix(p.velocity, swirl, 0.04)) * clamp(length(swirl) * 0.25, 0.1, 0.9);
			p.position = p.position + p.velocity * params.dt;
			if (abs(p.position.x) > 1.0) { p.velocity.x = -p.velocity.x; p.position.x = clamp(p.position.x, -1.0, 1.0); }
			if (abs(p.position.y) > 1.0) { p.velocity.y = -p.velocity.y; p.position.y = clamp(p.position.y, -1.0, 1.0); }
			particles[i] = p;
		}

		struct Out { @builtin(position) position: vec4<f32>, @location(0) colour: vec3<f32> };

		@vertex
		fn vs(@builtin(vertex_index) corner: u32, @builtin(instance_index) index: u32,
				@location(0) position: vec2<f32>, @location(1) velocity: vec2<f32>) -> Out {
			let corners = array<vec2<f32>, 3>(vec2(0.018, 0.0), vec2(-0.012, 0.008), vec2(-0.012, -0.008));
			let c = corners[corner];
			let a = atan2(velocity.y, velocity.x);
			let turned = vec2<f32>(c.x * cos(a) - c.y * sin(a), c.x * sin(a) + c.y * cos(a));
			var out: Out;
			out.position = vec4<f32>(position + turned * vec2<f32>(params.aspect, 1.0), 0.0, 1.0);
			let t = f32(index) / params.count + params.time * 0.05;
			out.colour = 0.55 + 0.45 * cos(6.2831 * (vec3<f32>(0.0, 0.33, 0.67) + t));
			return out;
		}

		@fragment
		fn fs(v: Out) -> @location(0) vec4<f32> {
			return vec4<f32>(v.colour, 1.0);
		}
	';

	static function main() {
		var instance = new GpuInstance();
		// In a page, the surface is the page's canvas.
		var surface = instance.surface(0, 0, 0, 0, 0);
		var adapter = instance.requestAdapter(HighPerformance).await();
		var device = adapter.requestDevice().await();
		var queue = device.queue();
		var format = surface.preferredFormat(adapter);
		var width = 640, height = 360;
		device.configureSurface(surface, width, height, format);
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
		var params = device.createBuffer(new GpuBufferDescriptor(16, BufferUsage.UNIFORM() | BufferUsage.COPY_DST()));

		var shader = device.createShader(SHADER);
		var stage = new GpuProgrammableStage(shader);
		stage.entryPoint("step");
		var stepping = device.createComputePipeline(new GpuComputePipelineDescriptor(stage));
		var group = new GpuBindGroupDescriptor(stepping.getBindGroupLayout(0));
		var particlesEntry = new GpuBindGroupEntry(0);
		particlesEntry.resourceBuffer(particles);
		group.addEntries(particlesEntry);
		var paramsEntry = new GpuBindGroupEntry(1);
		paramsEntry.resourceBuffer(params);
		group.addEntries(paramsEntry);
		var bound = device.createBindGroup(group);

		// The particles again, read as each arrow's instance data.
		var builder = device.pipeline();
		builder.shader(shader, "vs", "fs");
		builder.vertexBuffer(16, VertexStepMode.Instance);
		builder.attribute(VertexFormat.Float32x2, 0, 0);
		builder.attribute(VertexFormat.Float32x2, 8, 1);
		builder.target(format, ColorWrite.ALL());
		var drawing = builder.build();
		var drawGroup = new GpuBindGroupDescriptor(drawing.getBindGroupLayout(0));
		var drawParams = new GpuBindGroupEntry(1);
		drawParams.resourceBuffer(params);
		drawGroup.addEntries(drawParams);
		var drawBound = device.createBindGroup(drawGroup);

		var values = haxe.io.Bytes.alloc(16);
		var start = haxe.Timer.stamp(), last = start;
		var frames = 0;
		while (true) {
			var now = haxe.Timer.stamp();
			values.setFloat(0, Math.min(now - last, 0.05));
			values.setFloat(4, now - start);
			values.setFloat(8, height / width);
			values.setFloat(12, COUNT);
			last = now;
			queue.writeBuffer(params, 0, values, 16);

			var view = surface.acquire();
			var encoder = device.encoder();
			encoder.computeBegin();
			encoder.computeSetPipeline(stepping);
			encoder.computeSetBindGroup(0, bound);
			encoder.computeDispatch(Std.int(COUNT / 64), 1, 1);
			encoder.computeEnd();
			encoder.passColour(view, 0.02, 0.02, 0.05, 1.0);
			encoder.passBegin();
			encoder.renderSetPipeline(drawing);
			encoder.renderSetBindGroup(0, drawBound);
			encoder.renderSetVertexBuffer(0, particles);
			encoder.renderDraw(3, COUNT);
			encoder.renderEnd();
			encoder.submit(queue);
			queue.presentSurface(surface);
			if (++frames % 600 == 0) trace('$frames frames');
		}
	}
}
