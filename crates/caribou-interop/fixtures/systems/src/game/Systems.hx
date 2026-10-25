package game;

import haxe.io.Bytes;

/** Systems another language registers, each a typed function the engine
	keeps and calls once a step with the system's column and the step's
	time, as an engine runs its script systems. **/
@:keep
class Systems {
	static var systems:Array<(Bytes, Float) -> Void> = [];

	public static function register(system:(Bytes, Float) -> Void):Void {
		systems.push(system);
	}

	public static function count():Int {
		return systems.length;
	}

	static var failing:Array<(Bytes, Float) -> Void> = [];

	public static function registerFailing(system:(Bytes, Float) -> Void):Void {
		failing.push(system);
	}

	/** Calls each failing system once inside a catch, as an engine keeps a
		system's error from stopping the step, and answers what each raised,
		comma-separated. **/
	public static function faults():String {
		var column = Bytes.alloc(8);
		var raised = [];
		for (system in failing)
			try {
				system(column, 0.5);
				raised.push("nothing");
			} catch (e:Dynamic) {
				raised.push(Std.string(e));
			}
		return raised.join(",");
	}

	/** Steps every system `steps` times, collecting after each step, and
		answers what each wrote to its column, its step count at byte 0 and
		the step's time as an f32 at byte 4, as `count:dt`, comma-separated. **/
	public static function run(steps:Int):String {
		var columns = [for (_ in systems) Bytes.alloc(8)];
		for (_ in 0...steps) {
			for (i in 0...systems.length)
				systems[i](columns[i], 0.5);
			hl.Gc.major();
		}
		return [for (column in columns) column.get(0) + ":" + column.getFloat(4)].join(",");
	}
}
