import game.mover.Mover;

// A Haxe engine keeping a Python behaviour per entity: each made by its
// class, stepped each frame, a collection running between frames, and
// read back by its field.
class UseBehaviour {
	static function main() {
		var movers = [for (i in 0...100) new Mover(i)];
		for (_ in 0...3) {
			for (m in movers)
				m.step(0.5);
			hl.Gc.major();
		}
		var sum = 0.0;
		for (m in movers)
			sum += m.x;
		Sys.println(sum);
		Sys.println(movers[10] == movers[10] && movers[10] != movers[11]);
	}
}
