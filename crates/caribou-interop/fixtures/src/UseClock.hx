import core.Clock;

// A Haxe program stepping the world's clock: its threads' sleeps end when
// the program moves the clock on, the earlier deadline first, while main's
// own sleeps run them on wall time.
class UseClock {
	static function main() {
		Clock.step();
		var start = Clock.now();
		var woke = [];
		sys.thread.Thread.create(() -> {
			Sys.sleep(1.0);
			woke.push("late");
		});
		sys.thread.Thread.create(() -> {
			Sys.sleep(0.5);
			woke.push("first");
		});
		Sys.sleep(0.05);
		Sys.println(woke.length + " " + (Clock.now() - start));
		Clock.advance(0.5);
		Sys.sleep(0.01);
		Sys.println(woke.join(","));
		Clock.advance(0.5);
		Sys.sleep(0.01);
		Sys.println(woke.join(","));
		Sys.println(Clock.stepped());
		Clock.wall();
		Sys.println(Clock.stepped());
	}
}
