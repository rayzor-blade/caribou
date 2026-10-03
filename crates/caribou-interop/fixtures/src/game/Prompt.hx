package game;

// The host side of the effects test: the ZynML handler for `Ask` answers
// through this class.
@:keep
class Prompt {
	/** How many questions the ZynML handler put. */
	public static var asked:Int = 0;

	public static function question(q:String):String {
		asked++;
		return 'Ada, to "$q"';
	}

	public static function refuse(q:String):String {
		throw 'no answer to "$q"';
	}
}
