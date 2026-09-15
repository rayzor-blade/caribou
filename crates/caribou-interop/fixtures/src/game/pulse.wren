// The Wren side of the frame test: on the classpath at game/pulse.wren,
// reached from Haxe as `game.pulse.Pulse`. A fiber that counts on sleeps
// while the Haxe program runs its frame loop.
var count = 0

class Pulse {
  #export = "start(n: Num)"
  static start(n) {
    Fiber.spawn {
      for (i in 1..n) {
        Fiber.sleep(1)
        count = count + 1
      }
    }
  }

  #export = "count() -> Num"
  static count() { count }
}
