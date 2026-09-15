import game.tune.Tune;

// The Haxe side of the watch test: the program runs a frame loop and
// reads a Wren value each frame; the Wren file is edited meanwhile, and
// the value changes with no call of the program's own.
class UseWatch {
	@:hlNative("std", "sys_set_loop") static function setLoop(f:Void->Void):Void {}

	static var frames = 0;

	static function main() {
		Sys.println("value " + Tune.value());
		setLoop(function() {
			frames++;
			var v = Tune.value();
			if (v == 1 && frames < 600)
				return;
			Sys.println("value " + v + (frames < 600 ? "" : " after 600 frames"));
			// ash's frame loop ends on an error the frame function raises.
			throw "done";
		});
	}
}
