package game;

// The Haxe compiler's dead code elimination drops members nothing in Main
// references; another language will.
@:keep
class Player {
	/** How many `spawnAt` made: a static field, one storage for every
		language. */
	public static var spawned:Int = 0;

	/** Fired on every hit, when set: a typed callback any language may
		set. */
	public static var onHit:Int->Void;

	public var hp:Int = 100;
	public var name:String;

	public function new(name:String) {
		this.name = name;
	}

	public function hit(dmg:Int):Bool {
		hp -= dmg;
		if (onHit != null)
			onHit(dmg);
		return hp <= 0;
	}

	/** A typed callback parameter. */
	public static function twice(f:Int->Int, n:Int):Int {
		return f(f(n));
	}

	/** An untyped one. */
	public static function apply(f:Dynamic, x:Dynamic):Dynamic {
		return f(x);
	}

	public function explode():Void {
		throw "kaboom";
	}

	public static function spawnAt(x:Float, y:Float):Player {
		var p = new Player("spawned");
		p.hp = Std.int(x + y);
		spawned++;
		return p;
	}

	/** Arrays of objects and of numbers, for the other language to walk. */
	public static function party():Array<Player> {
		return [new Player("ann"), new Player("ben")];
	}

	public static function scores():Array<Int> {
		return [3, 1, 4];
	}

	/** The sum of what the other language handed back. */
	public static function total(xs:Array<Int>):Int {
		var t = 0;
		for (x in xs) t += x;
		return t;
	}
}
