import bench.tally.Tally;

// The interop benchmark's Haxe side. Each static loops `n` times over
// one operation and returns a value the loop depends on, so nothing is
// optimised away. The `haxe*` loops are the baseline, the same operation
// inside Haxe; the `wren*` loops do it against the Wren class Tally. The
// harness (caribou-interop/benches/interop.rs) calls them by name.
@:keep
class Bench {
	public var v:Float = 0;

	public function new() {}

	public static function add(x:Float):Float {
		return x + 1;
	}

	public function bump(x:Float):Float {
		v += x;
		return v;
	}

	/** A Haxe function for Wren to call. */
	public static function adder():Float->Float {
		return function(x:Float):Float return x + 1;
	}

	// Baselines.

	public static function haxeStatic(n:Int):Float {
		var s = 0.0;
		for (i in 0...n)
			s = add(s);
		return s;
	}

	public static function haxeMethod(n:Int):Float {
		var b = new Bench();
		for (i in 0...n)
			b.bump(1);
		return b.v;
	}

	public static function haxeGetter(n:Int):Float {
		var b = new Bench();
		b.v = 3;
		var s = 0.0;
		for (i in 0...n)
			s += b.v;
		return s;
	}

	public static function haxeSetter(n:Int):Float {
		var b = new Bench();
		for (i in 0...n)
			b.v = i;
		return b.v;
	}

	public static function haxeClosure(n:Int):Float {
		var f = adder();
		var s = 0.0;
		for (i in 0...n)
			s = f(s);
		return s;
	}

	public static function haxeNew(n:Int):Float {
		var b = null;
		for (i in 0...n)
			b = new Bench();
		return b.v;
	}

	// Into Wren.

	public static function wrenStatic(n:Int):Float {
		var s = 0.0;
		for (i in 0...n)
			s = Tally.add(s);
		return s;
	}

	public static function wrenMethod(n:Int):Float {
		var t = new Tally(0);
		for (i in 0...n)
			t.bump(1);
		return t.total;
	}

	public static function wrenGetter(n:Int):Float {
		var t = new Tally(3);
		var s = 0.0;
		for (i in 0...n)
			s += t.total;
		return s;
	}

	public static function wrenSetter(n:Int):Float {
		var t = new Tally(0);
		for (i in 0...n)
			t.total = i;
		return t.total;
	}

	public static function wrenClosure(n:Int):Float {
		var f = Tally.adder();
		var s = 0.0;
		for (i in 0...n)
			s = f(s);
		return s;
	}

	public static function wrenNew(n:Int):Float {
		var t = null;
		for (i in 0...n)
			t = new Tally(i);
		return t.total;
	}

	// Objects, strings and sequences crossing (benches/transfer.rs).

	static var kept:Bench = new Bench();
	static var last:Tally;

	public static function make():Bench {
		return new Bench();
	}

	public static function same():Bench {
		return kept;
	}

	public static function keep(t:Tally):Float {
		last = t;
		return 0;
	}

	public static function echo(s:String):String {
		return s;
	}

	public static function sum(xs:caribou.Sequence<Float>):Float {
		var t = 0.0;
		for (x in xs)
			t += x;
		return t;
	}

	public static function wrenSame(n:Int):Float {
		var b = new Bench();
		var s = 0.0;
		for (i in 0...n)
			s += Tally.take(b);
		return s;
	}

	public static function wrenFresh(n:Int):Float {
		var s = 0.0;
		for (i in 0...n)
			s += Tally.take(new Bench());
		return s;
	}

	public static function wrenReturned(n:Int):Float {
		var t = null;
		for (i in 0...n)
			t = Tally.make();
		return t.total;
	}

	public static function wrenString(n:Int):Float {
		var s = "hello";
		var len = 0;
		for (i in 0...n)
			len += Tally.echo(s).length;
		return len;
	}

	public static function wrenSequence(n:Int):Float {
		var xs = [for (i in 0...100) i * 1.0];
		var t = 0.0;
		for (i in 0...Std.int(n / 100))
			t += Tally.sum(xs);
		return t;
	}

	static function main() {}
}
