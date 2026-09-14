import swarm.World;
import swarm.Entity;
import swarm.game.Boid;
import swarm.game.Game;
import swarm.arena.Arena;

// The Swarm arena, three ways, for the benchmark harness
// (caribou-interop/benches/swarm.rs): the engine in Haxe with the
// behaviour in Wren (`runMixed`), the same behaviour in Haxe (`runHaxe`),
// and the whole arena in Wren (`runWren`, arena.wren). Each returns the
// same checksum for the same entity count and frames. Main is empty; the
// harness calls the runs by name.
@:keep
class Swarm {
	static function main() {}

	/** The engine's frame loop over Wren behaviour. */
	public static function runMixed(n:Int, frames:Int):Float {
		World.reset(12345);
		Game.reset();

		var boids:Array<Boid> = [];
		for (i in 0...n) {
			var e = World.spawnRandom();
			boids.push(new Boid(e, e.index));
		}
		var dt = 1.0 / 60;
		var acc = 0.0;
		var tween:Float->Float = null;
		var hud = 0;
		for (frame in 0...frames) {
			for (b in boids)
				acc += b.update(dt);
			World.step(dt);
			var c = World.collisions();
			for (k in 0...c)
				Game.onCollide(World.collisionA(k), World.collisionB(k));
			var spawn = Std.int(Game.spawnCount(frame));
			for (s in 0...spawn) {
				var e = World.spawnRandom();
				boids.push(new Boid(e, e.index));
			}
			if (frame % 60 == 0) {
				tween = Game.waveTween(frame);
				hud += Game.hud().length;
			}
			if (tween != null)
				acc += tween(frame * dt);
		}
		return World.checksum() + acc + Game.score() + hud;
	}

	/** The same loop over the Haxe behaviour below. */
	public static function runHaxe(n:Int, frames:Int):Float {
		World.reset(12345);
		GameH.reset();
		var boids:Array<BoidH> = [];
		for (i in 0...n) {
			var e = World.spawnRandom();
			boids.push(new BoidH(e, e.index));
		}
		var dt = 1.0 / 60;
		var acc = 0.0;
		var tween:Float->Float = null;
		var hud = 0;
		for (frame in 0...frames) {
			for (b in boids)
				acc += b.update(dt);
			World.step(dt);
			var c = World.collisions();
			for (k in 0...c)
				GameH.onCollide(World.collisionA(k), World.collisionB(k));
			var spawn = Std.int(GameH.spawnCount(frame));
			for (s in 0...spawn) {
				var e = World.spawnRandom();
				boids.push(new BoidH(e, e.index));
			}
			if (frame % 60 == 0) {
				tween = GameH.waveTween(frame);
				hud += GameH.hud().length;
			}
			if (tween != null)
				acc += tween(frame * dt);
		}
		return World.checksum() + acc + GameH.score() + hud;
	}

	/** The whole arena in Wren. */
	public static function runWren(n:Int, frames:Int):Float {
		return Arena.run(n, frames);
	}
}

/** game.wren's Boid, in Haxe. */
@:keep
class BoidH {
	var e:Entity;
	var i:Int;

	public function new(e:Entity, i:Int) {
		this.e = e;
		this.i = i;
	}

	public function update(dt:Float):Float {
		var e = this.e;
		var i = this.i;
		var x = e.x;
		var y = e.y;
		var vx = e.vx;
		var vy = e.vy;
		var n = World.neighborCount(i);
		var cx = 0.0;
		var cy = 0.0;
		var avx = 0.0;
		var avy = 0.0;
		var sx = 0.0;
		var sy = 0.0;
		var k = 0;
		while (k < n) {
			var j = World.neighborAt(i, k);
			var dx = World.x(j) - x;
			var dy = World.y(j) - y;
			cx = cx + dx;
			cy = cy + dy;
			avx = avx + World.vx(j);
			avy = avy + World.vy(j);
			if (dx * dx + dy * dy < 100) {
				sx = sx - dx;
				sy = sy - dy;
			}
			k = k + 1;
		}
		if (n > 0) {
			var ax = cx / n * 0.02 + (avx / n - vx) * 0.05 + sx * 0.1;
			var ay = cy / n * 0.02 + (avy / n - vy) * 0.05 + sy * 0.1;
			vx = vx + ax * dt * 60;
			vy = vy + ay * dt * 60;
			if (vx > 60)
				vx = 60;
			if (vx < -60)
				vx = -60;
			if (vy > 60)
				vy = 60;
			if (vy < -60)
				vy = -60;
			e.vx = vx;
			e.vy = vy;
		}
		return vx + vy;
	}
}

/** game.wren's Game, in Haxe. */
@:keep
class GameH {
	static var scoreCount:Int = 0;

	public static function reset():Void {
		scoreCount = 0;
	}

	public static function score():Float {
		return scoreCount;
	}

	public static function onCollide(a:Int, b:Int):Void {
		World.bounce(a, b);
		scoreCount++;
	}

	public static function spawnCount(frame:Int):Float {
		return frame % 30 == 0 ? 2 : 0;
	}

	public static function waveTween(frame:Int):Float->Float {
		var scale = frame % 7 + 1;
		return function(t:Float):Float return t * scale;
	}

	public static function hud():String {
		return "score " + scoreCount;
	}
}
