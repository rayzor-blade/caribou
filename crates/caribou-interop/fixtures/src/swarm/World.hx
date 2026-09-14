package swarm;

// The arena's engine: the entities, a grid over them, each entity's
// nearest neighbours and the collisions of a step, all indexed by entity
// number. Scripts read the world through the statics here and an entity's
// own fields, and never hold a table of it. The arena wraps.
@:keep
class World {
	public static inline var SIZE:Float = 1000;
	public static inline var CELL:Float = 50;
	public static inline var CELLS:Int = 20;
	public static inline var RADIUS:Float = 4;
	public static inline var NEIGHBOURS:Int = 8;
	public static inline var SPEED:Float = 60;

	public static var entities:Array<Entity> = [];
	static var seed:Float = 1;
	// Neighbours per entity, NEIGHBOURS slots each, and how many are in use.
	static var nb:Array<Int> = [];
	static var nbCount:Array<Int> = [];
	// The grid as chains: the first entity of each cell, and each entity's
	// next in its cell.
	static var head:Array<Int> = [];
	static var next:Array<Int> = [];
	// The collisions the last step found, as pairs.
	static var collA:Array<Int> = [];
	static var collB:Array<Int> = [];
	static var collN:Int = 0;

	public static function reset(s:Int):Void {
		entities = [];
		seed = s;
		nb = [];
		nbCount = [];
		next = [];
		head = [];
		for (c in 0...CELLS * CELLS)
			head.push(-1);
		collA = [];
		collB = [];
		collN = 0;
	}

	/** Park-Miller: the same sequence in every language, exact in doubles. */
	public static function random():Float {
		seed = (seed * 16807) % 2147483647;
		return seed / 2147483647;
	}

	public static function spawn(x:Float, y:Float, vx:Float, vy:Float):Entity {
		var e = new Entity(entities.length, x, y, vx, vy);
		entities.push(e);
		for (k in 0...NEIGHBOURS)
			nb.push(-1);
		nbCount.push(0);
		next.push(-1);
		return e;
	}

	/** An entity at a random place with a random velocity. */
	public static function spawnRandom():Entity {
		var x = random() * SIZE;
		var y = random() * SIZE;
		var vx = (random() - 0.5) * SPEED;
		var vy = (random() - 0.5) * SPEED;
		return spawn(x, y, vx, vy);
	}

	public static function count():Int {
		return entities.length;
	}

	public static function x(i:Int):Float {
		return entities[i].x;
	}

	public static function y(i:Int):Float {
		return entities[i].y;
	}

	public static function vx(i:Int):Float {
		return entities[i].vx;
	}

	public static function vy(i:Int):Float {
		return entities[i].vy;
	}

	public static function neighborCount(i:Int):Int {
		return nbCount[i];
	}

	public static function neighborAt(i:Int, k:Int):Int {
		return nb[i * NEIGHBOURS + k];
	}

	public static function collisions():Int {
		return collN;
	}

	public static function collisionA(k:Int):Int {
		return collA[k];
	}

	public static function collisionB(k:Int):Int {
		return collB[k];
	}

	/** Two entities exchange velocities. */
	public static function bounce(a:Int, b:Int):Void {
		var ea = entities[a];
		var eb = entities[b];
		var vx = ea.vx;
		var vy = ea.vy;
		ea.vx = eb.vx;
		ea.vy = eb.vy;
		eb.vx = vx;
		eb.vy = vy;
	}

	static inline function cellOf(x:Float, y:Float):Int {
		return Std.int(Math.floor(y / CELL)) * CELLS + Std.int(Math.floor(x / CELL));
	}

	/** Move everything, rebuild the grid, find neighbours and collisions. */
	public static function step(dt:Float):Void {
		var n = entities.length;
		for (i in 0...n) {
			var e = entities[i];
			var x = e.x + e.vx * dt;
			var y = e.y + e.vy * dt;
			if (x < 0)
				x += SIZE;
			if (x >= SIZE)
				x -= SIZE;
			if (y < 0)
				y += SIZE;
			if (y >= SIZE)
				y -= SIZE;
			e.x = x;
			e.y = y;
		}
		for (c in 0...CELLS * CELLS)
			head[c] = -1;
		// Chained from the back, so a cell lists its entities in ascending
		// order.
		var i = n - 1;
		while (i >= 0) {
			var e = entities[i];
			var c = cellOf(e.x, e.y);
			next[i] = head[c];
			head[c] = i;
			i--;
		}
		for (i in 0...n) {
			var e = entities[i];
			var cx = Std.int(Math.floor(e.x / CELL));
			var cy = Std.int(Math.floor(e.y / CELL));
			var found = 0;
			var dy = -1;
			while (dy <= 1 && found < NEIGHBOURS) {
				var dx = -1;
				while (dx <= 1 && found < NEIGHBOURS) {
					var c = ((cy + dy + CELLS) % CELLS) * CELLS + ((cx + dx + CELLS) % CELLS);
					var j = head[c];
					while (j >= 0 && found < NEIGHBOURS) {
						if (j != i) {
							nb[i * NEIGHBOURS + found] = j;
							found++;
						}
						j = next[j];
					}
					dx++;
				}
				dy++;
			}
			nbCount[i] = found;
		}
		collN = 0;
		for (c in 0...CELLS * CELLS) {
			var a = head[c];
			while (a >= 0) {
				var b = next[a];
				while (b >= 0) {
					var ea = entities[a];
					var eb = entities[b];
					var dx = ea.x - eb.x;
					var dy = ea.y - eb.y;
					if (dx * dx + dy * dy < 4 * RADIUS * RADIUS) {
						if (collN < collA.length) {
							collA[collN] = a;
							collB[collN] = b;
						} else {
							collA.push(a);
							collB.push(b);
						}
						collN++;
					}
					b = next[b];
				}
				a = next[a];
			}
		}
	}

	/** What the run is checked by: every entity's state, summed in order. */
	public static function checksum():Float {
		var s = 0.0;
		for (e in entities)
			s += e.x + e.y + e.vx + e.vy;
		return s;
	}
}
