import game.hud.Hud;

// The Haxe side of the Haxe-imports-Wren test: `game.hud.Hud` is the class
// the build macro emitted for wren/hud.wren. The test compares what this
// prints, then collects on both sides and calls `after`.
class UseHud {
	static var kept:Hud;
	static var keptFn:Float->Float;

	static function main() {
		var h = new Hud(3);
		kept = h;
		Sys.println(h.add(4));
		h.score = 10;
		Sys.println(h.score);
		Sys.println(h.label("hp"));
		var m = Hud.make(1);
		// An object Wren made is a Hud to Haxe's own type tests and casts.
		Sys.println(m.score + " " + Std.isOfType(m, Hud) + " " + (Type.getClass(m) == Hud) + " " + Std.isOfType(m, caribou.Ref));
		var d:Dynamic = m;
		Sys.println((d : Hud) == m && Std.isOfType(d, Hud) && !Std.isOfType(h, Array));
		var b = Hud.best(h, m);
		Sys.println(b == h);
		Sys.println(b.score);
		try {
			h.explode();
		} catch (e:String) {
			Sys.println("caught " + e);
		}
		Sys.println(h.owner());
		Hud.count = 5;
		Sys.println(Hud.count);
		// A Haxe function Wren keeps and calls later, and one it calls at once.
		Hud.onTick(function(n:Dynamic):Dynamic return n * 2);
		Sys.println(Hud.tick(21));
		Sys.println(Hud.twice(function(x:Float):Float return x + 1, 1));
		// A Wren function Haxe keeps, typed, and calls after both collectors.
		keptFn = Hud.scaler(3);
		Sys.println(keptFn(2));
		// A Wren list, read and written where Wren keeps it; a Haxe array
		// handed to Wren as it is.
		var labels = Hud.labels();
		Sys.println(labels.length + " " + labels[0]);
		labels[2] = 4;
		var seen = [for (x in labels) Std.string(x)].join(",");
		Sys.println(seen);
		Sys.println(Hud.sum([1, 2, 3.5]));
		labels[0] = 10;
		Sys.println(Hud.labelCount() + " " + Hud.sum(labels.toArray().slice(0, 1)));
	}

	@:keep static function after() {
		Sys.println(kept.label("after"));
		Sys.println(keptFn(4));
	}
}
