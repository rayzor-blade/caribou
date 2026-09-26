import game.counter.Counter;

// Haxe using a Lua class: `game.counter.Counter` is the class the build
// macro emitted for the one the Lua module `game/counter.lua` returns,
// typed by its LuaLS annotations. An instance is made by its `new`, its
// `:` functions are its methods, its `@field`s are its fields, and the
// class's other fields are statics.
class UseLua {
	static function main() {
		var c = new Counter(3);
		Sys.println(c.n);
		Sys.println(c.bump(4));
		Sys.println(c.bump(9));
		Sys.println(Counter.LIMIT);
		Counter.LIMIT = 20;
		Sys.println(c.bump(5));
		c.n = 2;
		Sys.println(c.sum(i -> i * 10));
		Sys.println(c.label);
		Sys.println(Counter.checksum(haxe.io.Bytes.ofString("abc")));
		var add = Counter.adder(5);
		Sys.println(add(2));
		var d:Counter = c.next();
		Sys.println(d.bump(1));
		// Several results are an anonymous structure of them.
		var s = c.state();
		Sys.println(s.count + " " + s.label);
		Sys.println(Counter.parse("12").count);
		Sys.println(Counter.parse("x").error);
	}
}
