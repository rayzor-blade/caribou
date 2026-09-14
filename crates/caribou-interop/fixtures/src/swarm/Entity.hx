package swarm;

// One entity of the arena: its state as plain fields, which are what a
// script reads and writes, and its index in the world, which is what the
// world's own tables are keyed by.
@:keep
class Entity {
	public var index:Int;
	public var x:Float;
	public var y:Float;
	public var vx:Float;
	public var vy:Float;

	public function new(index:Int, x:Float, y:Float, vx:Float, vy:Float) {
		this.index = index;
		this.x = x;
		this.y = y;
		this.vx = vx;
		this.vy = vy;
	}
}
