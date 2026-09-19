import game.tally.Tally;

// The Haxe side of the Python test: `game.tally.Tally` is the class the
// build macro emitted for the class the Python module `game/tally.py`
// declares, under the module as its package. The module's functions are
// not types, so Haxe does not see them; an instance cannot cross yet, so
// the program only names the type.
class UsePython {
	static function main() {
		Sys.println(Type.getClassName(Tally));
	}
}
