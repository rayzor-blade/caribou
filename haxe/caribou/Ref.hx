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
}
