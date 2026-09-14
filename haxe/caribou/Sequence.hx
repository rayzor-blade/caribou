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

	// Every array kind extends `ArrayBase`, and a class with no
	// implementers is one walk up the object's class chain.
	inline function isArray():Bool {
		return Std.isOfType(this, hl.types.ArrayBase);
	}

	inline function get_length():Int {
		return isArray() ? (this : Array<T>).length : Ref.__len(this);
	}

	@:arrayAccess public inline function get(i:Int):T {
		return isArray() ? (this : Array<T>)[i] : cast Ref.__index(this, i);
	}

	@:arrayAccess public inline function set(i:Int, v:T):T {
		if (isArray()) {
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
		if (isArray()) {
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

/** Decides once which kind it walks; the length is read per step, as
	the sequence may change under the walk. Both fields start null
	explicitly: inlined, they are locals, and HashLink does not clear
	one that is never assigned. */
private class SequenceIterator<T> {
	var array:Array<T> = null;
	var ref:Dynamic = null;
	var i = 0;

	public inline function new(xs:Dynamic) {
		if (Std.isOfType(xs, hl.types.ArrayBase)) {
			array = xs;
		} else {
			ref = xs;
		}
	}

	public inline function hasNext():Bool {
		return i < (array != null ? array.length : Ref.__len(ref));
	}

	public inline function next():T {
		return array != null ? array[i++] : cast Ref.__index(ref, i++);
	}
}
