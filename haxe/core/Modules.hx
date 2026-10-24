package core;

/**
	Module lifecycle. `onReload(handler)` hears each module's reload:
	`handler(namespace, module, true, null)` once the new source compiled
	and before its body runs, then `handler(namespace, module, false,
	error)` after, `error` null when the reload worked. A source that does
	not compile is heard only after, with its error, and the module stays
	as it was. A host drops what a module registered on the `true` call.
**/
@:keep
class Modules extends caribou.Ref {
	@:hlNative("caribou", "core:Modules.Modules.static:onReload(_)")
	public static function onReload(handler:(String, String, Bool, Null<String>) -> Void):Void {}
}
