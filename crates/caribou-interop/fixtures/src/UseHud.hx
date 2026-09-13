import game.hud.Hud;

// The Haxe side of the Haxe-imports-Wren test: `game.hud.Hud` is the class
// the build macro emitted for wren/hud.wren. The test compares what this
// prints, then collects on both sides and calls `after`.
class UseHud {
	static var kept:Hud;

	static function main() {
		var h = new Hud(3);
		kept = h;
		Sys.println(h.add(4));
		h.score = 10;
		Sys.println(h.score);
		Sys.println(h.label("hp"));
		var m = Hud.make(1);
		var b = Hud.best(h, m);
		Sys.println(b == h);
		Sys.println(b.score);
		try {
			h.explode();
		} catch (e:String) {
			Sys.println("caught " + e);
		}
	}

	@:keep static function after() {
		Sys.println(kept.label("after"));
	}
}
