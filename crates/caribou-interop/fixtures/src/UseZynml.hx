import game.scorer.Point;

// The Haxe side of the Zyntax test: `game.scorer.Point` is the class the
// build macro emitted for the struct the ZynML module `game/scorer.zynml`
// declares, under the module as its package. The module's functions are
// the module's own and not types, so Haxe does not see them; the
// struct's statics over scalars are called, and an instance cannot cross
// yet.
class UseZynml {
	static function main() {
		Sys.println(Point.area(3, 4));
		try {
			Sys.println(Point.origin());
		} catch (e:Dynamic) {
			Sys.println("caught " + e);
		}
	}
}
