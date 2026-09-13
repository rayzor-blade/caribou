package game;

// The Haxe compiler's dead code elimination drops members nothing in Main
// references; another language will.
@:keep
class Player {
	/** How many `spawnAt` made: a static field, one storage for every
		language. */
	public static var spawned:Int = 0;

	public var hp:Int = 100;
	public var name:String;

	public function new(name:String) {
		this.name = name;
	}

	public function hit(dmg:Int):Bool {
		hp -= dmg;
		return hp <= 0;
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
}
