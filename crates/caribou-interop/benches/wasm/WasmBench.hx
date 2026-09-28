import bench.tally.Tally;

// The interop benchmark built ahead of time for wasm (benches/
// interop_wasm.rs): the same cells as interop.rs, over the same Bench and
// Tally, each one operation looped `n` times inside the calling
// language's function and timed here as one call. The median of `runs`
// runs is printed in nanoseconds per iteration. A cell whose call does
// not link yet raises, and is left blank.
class WasmBench {
	static function main() {
		attachmentProbe();
		var args = Sys.args();
		var n = args.length > 0 ? Std.parseInt(args[0]) : 200000;
		var runs = args.length > 1 ? Std.parseInt(args[1]) : 5;
		var columns = ["Haxe→Haxe", "Wren→Wren", "Haxe→Wren", "Wren→Haxe"];
		var rows:Array<{label:String, cells:Array<Int->Float>}> = [
			{label: "static call", cells: [n -> Bench.haxeStatic(n), n -> Tally.wrenStatic(n), n -> Bench.wrenStatic(n), n -> Tally.haxeStatic(n)]},
			{label: "method call", cells: [n -> Bench.haxeMethod(n), n -> Tally.wrenMethod(n), n -> Bench.wrenMethod(n), n -> Tally.haxeMethod(n)]},
			{label: "getter", cells: [n -> Bench.haxeGetter(n), n -> Tally.wrenGetter(n), n -> Bench.wrenGetter(n), n -> Tally.haxeGetter(n)]},
			{label: "setter", cells: [n -> Bench.haxeSetter(n), n -> Tally.wrenSetter(n), n -> Bench.wrenSetter(n), n -> Tally.haxeSetter(n)]},
			{label: "closure call", cells: [n -> Bench.haxeClosure(n), n -> Tally.wrenClosure(n), n -> Bench.wrenClosure(n), n -> Tally.haxeClosure(n)]},
			{label: "construct", cells: [n -> Bench.haxeNew(n), n -> Tally.wrenNew(n), n -> Bench.wrenNew(n), n -> Tally.haxeNew(n)]},
		];
		Sys.println('wasm, $n iterations, median of $runs runs, ns per call\n');
		var line = StringTools.rpad("", " ", 14);
		for (c in columns)
			line += StringTools.lpad(c, " ", 12);
		Sys.println(line);
		for (row in rows) {
			var line = StringTools.rpad(row.label, " ", 14);
			for (cell in row.cells)
				line += StringTools.lpad(measure(cell, n, runs), " ", 12);
			Sys.println(line);
		}
	}

	static function attachmentProbe() {
		var live = new Tally(41);
		if (Tally.same(live) != live)
			throw "a live Wren object did not keep its Haxe face";
		Tally.keep(live);
		live = null;
		hl.Gc.major();
		var restored = Tally.kept();
		if (restored.total != 41)
			throw "a Wren object did not survive its Haxe face";
		Tally.clear();
	}

	static function measure(cell:Int->Float, n:Int, runs:Int):String {
		function once():Float {
			var started = haxe.Timer.stamp();
			cell(n);
			return (haxe.Timer.stamp() - started) * 1e9 / n;
		}
		try {
			once();
			once();
		} catch (e:Dynamic) {
			return "";
		}
		var samples = [for (_ in 0...runs) once()];
		samples.sort(Reflect.compare);
		return Std.string(Math.round(samples[samples.length >> 1] * 10) / 10);
	}
}
