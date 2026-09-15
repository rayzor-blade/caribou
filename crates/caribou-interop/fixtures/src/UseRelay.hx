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
		// A Haxe task parked inside a Wren call, its Wren frame's data held
		// by nothing else, while a Wren cycle runs and the freed memory is
		// reused.
		var churned = 0.0;
		var churned2 = 0.0;
		var back = new Lock();
		Thread.create(function() {
			churned = Relay.churn(2000);
			back.release();
		});
		// A second task inside the same Wren call, parked beside the first:
		// each resumes into the run it was in.
		Thread.create(function() {
			churned2 = Relay.churn(1000);
			back.release();
		});
		Relay.cycle();
		back.wait();
		back.wait();
		Sys.println(churned + " " + churned2);
		// A Haxe throw on this stack while a task is parked inside a Haxe
		// try on another.
		try {
			Relay.nested();
			Sys.println("no throw");
		} catch (e:String) {
			Sys.println("caught " + e);
		}
		Sys.println(Relay.finish());
	}
}
