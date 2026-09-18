import game.scorer.Point;
import game.scorer.Scorer;

// The Haxe side of the Zyntax test: `game.scorer.Scorer` is the class
// the build macro emitted for the ZynML module under the classpath root,
// from the module's HIR, and the program reaches it as it reaches any
// class.
class UseZynml {
	static function main() {
		Sys.println(Scorer.score(7, 2));
		Sys.println(Scorer.weight(3, 1.5));
		Sys.println(Scorer.perfect(5, 5) + " " + Scorer.perfect(4, 5));
		Sys.println(Scorer.echo("goal"));
		// A struct the module declares is a class: its statics over
		// scalars are called; an instance cannot cross yet.
		Sys.println(Point.area(3, 4));
		try {
			Sys.println(Point.origin());
		} catch (e:Dynamic) {
			Sys.println("caught " + e);
		}
	}
}
