import math.Math;
import math.Vec2;

// The Haxe side of the plugin test: `math.Math` and `math.Vec2` are the
// classes the build macro emitted for the plugin in plugins/ beside the
// program, and the program reaches them as it reaches any class.
class UsePlugin {
	static function main() {
		Sys.println(Math.hypot(3, 4));
		Sys.println(Math.twice(21));
		Sys.println(Math.is_even(6));
		var v = new Vec2(3, 4);
		Sys.println(v.len());
		v.scale(2);
		Sys.println(v.len() + " " + v.dot(new Vec2(1, 0)));
		Sys.println(v.unit().len());
		Sys.println(Std.isOfType(v, Vec2) + " " + Std.isOfType(v.unit(), Vec2));
		// Haxe's own cast refuses another class before the plugin would.
		try {
			Sys.println(v.dot((cast "five" : Vec2)));
		} catch (e:Dynamic) {
			Sys.println("caught " + e);
		}
	}
}
