import core.Scope;

// A Haxe program cancelling the threads it started in a scope: their
// sleeps end at once, they see they were cancelled and leave, and a thread
// started outside the scope sleeps as it would.
class UseScope {
	static function main() {
		var log = [];
		var scope = Scope.open();
		var before = Scope.enter(scope);
		for (name in ["a", "b"])
			sys.thread.Thread.create(() -> {
				Sys.sleep(10);
				log.push(name + (Scope.cancelled() ? " cancelled" : " slept"));
			});
		Scope.enter(before);
		sys.thread.Thread.create(() -> {
			Sys.sleep(0.01);
			log.push("c" + (Scope.cancelled() ? " cancelled" : " slept"));
		});
		Sys.sleep(0.05);
		Sys.println(Scope.cancel(scope));
		Sys.sleep(0.05);
		log.sort(Reflect.compare);
		Sys.println(log.join(","));
	}
}
