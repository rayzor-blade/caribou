package core;

/**
	The world's clock. A host that runs a fixed-step simulation calls
	`step()` once, then `advance(dt)` each step; the timed waits of the
	world's tasks (`Sys.sleep` in a thread, a Wren `Timer.sleep`, a Zyntax
	`sleep`) then end by this clock, in the order they began when they
	share a deadline. The main thread's own waits keep wall time.
**/
@:keep
class Clock extends caribou.Ref {
	/** Step the clock from now on: it stands still until `advance`. **/
	@:hlNative("caribou", "core:Clock.Clock.static:step()")
	public static function step():Void {}

	/** Move a stepped clock on by `seconds`; false on wall time. **/
	@:hlNative("caribou", "core:Clock.Clock.static:advance(_)")
	public static function advance(seconds:Float):Bool {
		return false;
	}

	/** The world's time in seconds. **/
	@:hlNative("caribou", "core:Clock.Clock.static:now()")
	public static function now():Float {
		return 0.0;
	}

	/** Back to wall time; a wait under way keeps what it had left. **/
	@:hlNative("caribou", "core:Clock.Clock.static:wall()")
	public static function wall():Void {}

	@:hlNative("caribou", "core:Clock.Clock.static:stepped()")
	public static function stepped():Bool {
		return false;
	}
}
