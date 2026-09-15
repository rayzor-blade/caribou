import game.relay.Relay;
import sys.thread.Lock;
import sys.thread.Thread;

// The Haxe side of the one-world test: a Haxe thread, a Wren fiber and a
// Wren thread run on the one world, and a wait on either side lets the
// others run.
class UseRelay {
	static var log:Array<String> = [];

	static function main() {
		var lock = new Lock();
		Thread.create(function() {
			Sys.sleep(0.005);
			log.push("haxe-thread");
			lock.release();
		});
		Relay.start();
		Relay.wait();
		lock.wait();
		log.sort(Reflect.compare);
		Sys.println(log.join(","));
		var wren = Relay.log().toArray();
		wren.sort(Reflect.compare);
		Sys.println(wren.join(","));
		Sys.println(Relay.guarded());
	}
}
