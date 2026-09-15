import game.pulse.Pulse;

// The Haxe side of the frame test: the program installs a frame loop the
// way a UI library does, and a Wren fiber makes its progress in the
// frames' idle time.
class UseFrame {
	@:hlNative("std", "sys_set_loop") static function setLoop(f:Void->Void):Void {}

	static var frames = 0;

	static function main() {
		Pulse.start(5);
		setLoop(function() {
			frames++;
			if (Pulse.count() < 5 && frames < 200)
				return;
			Sys.println("count " + Pulse.count() + " in " + (frames < 200 ? "time" : "200 frames"));
			// ash's frame loop ends on an error the frame function raises.
			throw "done";
		});
	}
}
