// The Haxe side of the Haxe reload test: a program the test rebuilds with
// one constant changed while it runs. `answer` is what the test calls
// before and after; `warm` calls it enough times for the tier to compile
// it, so the reload has compiled code to replace.
class UseReload {
	static function main() {
		Sys.println(answer());
	}

	public static function answer():Int {
		return 41 + tick();
	}

	static function tick():Int {
		return 1;
	}

	public static function warm(n:Int):Int {
		var s = 0;
		for (i in 0...n) {
			s += answer();
		}
		return s;
	}
}
