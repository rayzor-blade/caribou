package core;

/**
	Task scopes: a host opens one, starts tasks in it, and cancels them
	together, as an editor stops a game it previews. A cancelled task's
	waits end at once; the code that waited asks `cancelled()` and leaves
	the way its language leaves, by throwing.
**/
@:keep
class Scope extends caribou.Ref {
	/** A new scope. **/
	@:hlNative("caribou", "core:Scope.Scope.static:open()")
	public static function open():Int {
		return 0;
	}

	/** Tasks started from here on join `scope`; returns the scope it
		replaces. **/
	@:hlNative("caribou", "core:Scope.Scope.static:enter(_)")
	public static function enter(scope:Int):Int {
		return 0;
	}

	/** The scope tasks started from here join; 0 for none. **/
	@:hlNative("caribou", "core:Scope.Scope.static:current()")
	public static function current():Int {
		return 0;
	}

	/** Cancel the scope's tasks; returns how many it reached. **/
	@:hlNative("caribou", "core:Scope.Scope.static:cancel(_)")
	public static function cancel(scope:Int):Int {
		return 0;
	}

	/** Whether the running task's scope was cancelled. **/
	@:hlNative("caribou", "core:Scope.Scope.static:cancelled()")
	public static function cancelled():Bool {
		return false;
	}
}
