package caribou;

/**
	A Haxe object standing for another language's object.

	The runtime keeps a ref for the object and stores it in the first
	field; nothing in Haxe reads that field. Every class the build macro
	emits for a foreign class extends this one, and a foreign object of a
	class the program does not declare arrives as a plain `Ref`.
**/
@:keep
class Ref {
	var __ref:hl.Abstract<"caribou_obj">;

	function new() {}

	// A sequence's elements and count, through the bridge, for
	// `caribou.Sequence`: the receiver is a ref, or a Haxe object the
	// bridge wraps.
	@:hlNative("caribou", "len") @:allow(caribou.Sequence) static function __len(seq:Dynamic):Int {
		return 0;
	}

	@:hlNative("caribou", "index") @:allow(caribou.Sequence) static function __index(seq:Dynamic, i:Int):Dynamic {
		return null;
	}

	@:hlNative("caribou", "set_index") @:allow(caribou.Sequence) static function __setIndex(seq:Dynamic, i:Int, v:Dynamic):Void {}
}
