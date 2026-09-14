package caribou;

/**
	A sequence of either language: a Haxe `Array<T>`, or another
	language's sequence, a Wren `List`, behind its ref. `xs[i]`,
	`xs[i] = v`, `xs.length` and `for (x in xs)` work on both; on a
	foreign one every access reaches the sequence where its language
	keeps it, so writes are seen on both sides and it goes back as
	itself. A Haxe array is a `Sequence` as it is, and a foreign one
	copies into an array only when asked (`toArray`).
**/
abstract Sequence<T>(Dynamic) from Array<T> {
	public var length(get, never):Int;

	inline function get_length():Int {
		return Std.isOfType(this, Array) ? (this : Array<T>).length : Ref.__len(this);
	}

	@:arrayAccess public inline function get(i:Int):T {
		return Std.isOfType(this, Array) ? (this : Array<T>)[i] : cast Ref.__index(this, i);
	}

	@:arrayAccess public inline function set(i:Int, v:T):T {
		if (Std.isOfType(this, Array)) {
			(this : Array<T>)[i] = v;
		} else {
			Ref.__setIndex(this, i, v);
		}
		return v;
	}

	public inline function iterator():Iterator<T> {
		return new SequenceIterator(this);
	}

	/** A Haxe array with the elements as they are now. */
	public function toArray():Array<T> {
		if (Std.isOfType(this, Array)) {
			return (this : Array<T>).copy();
		}
		var out = [];
		var n = length;
		for (i in 0...n) {
			out.push(get(i));
		}
		return out;
	}
}

private class SequenceIterator<T> {
	var xs:Sequence<T>;
	var i = 0;

	public inline function new(xs:Sequence<T>) {
		this.xs = xs;
	}

	public inline function hasNext():Bool {
		return i < xs.length;
	}

	public inline function next():T {
		return xs[i++];
	}
}
