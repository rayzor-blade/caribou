package caribou;

/**
	An eventual value shared by every Caribou language.

	`await()` parks the current Caribou fiber, so other language tasks on the
	same world continue to run. A rejected future throws its original error.
**/
@:keep
class Future<T> extends Ref {
	public function new() {
		super();
		__new(this);
	}

	@:hlNative("caribou", "future_new")
	static function __new(future:Dynamic):Void {}

	@:hlNative("caribou", "future_ready")
	static function __ready(future:Dynamic):Bool {
		return false;
	}

	@:hlNative("caribou", "future_await")
	static function __await(future:Dynamic):Dynamic {
		return null;
	}

	@:hlNative("caribou", "future_resolve")
	static function __resolve(future:Dynamic, value:Dynamic):Bool return false;

	@:hlNative("caribou", "future_reject")
	static function __reject(future:Dynamic, error:Dynamic):Bool return false;

	public inline function ready():Bool return __ready(this);
	public inline function await():T return cast __await(this);
	public inline function resolve(value:T):Bool return __resolve(this, value);
	public inline function reject(error:Dynamic):Bool return __reject(this, error);
}
