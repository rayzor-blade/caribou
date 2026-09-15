// The Wren side of the one-world test: on the classpath at
// game/relay.wren, reached from Haxe as `game.relay.Relay`. Its fibers
// and threads are tasks of the world Haxe's threads run on.
import "thread" for Thread, Lock
import "game:Player" for Player

var log = []
var lock = Lock.new()
var innerResult = null
var innerDone = Lock.new()

class Relay {
  // A fiber that sleeps and a thread that runs at once, each logging
  // and releasing the lock `wait` takes twice.
  #export = "start()"
  static start() {
    Fiber.spawn {
      Fiber.sleep(3)
      log.add("wren-fiber")
      lock.release()
    }
    Thread.create {
      log.add("wren-thread")
      lock.release()
    }
  }

  #export = "wait()"
  static wait() {
    lock.wait()
    lock.wait()
  }

  #export = "log() -> List"
  static log() { log }

  // Called from a Haxe task: what this frame holds across the park is
  // held by nothing else, and a Wren cycle runs meanwhile.
  #export = "churn(n: Num) -> Num"
  static churn(n) {
    var xs = []
    for (i in 0...n) xs.add([i, "%(i)"])
    Fiber.sleep(5)
    var t = 0
    for (x in xs) t = t + x[0] + x[1].count
    return t
  }

  // A cycle of Wren's own, and enough allocation after it to reuse what
  // it freed.
  #export = "cycle()"
  static cycle() {
    System.gc()
    var junk = []
    for (i in 0...4000) junk.add([i, "junk%(i)"])
  }

  // A task parked inside a Haxe try on its own stack while this run, on
  // the thread's own stack, throws from Haxe: each stack's traps are its
  // own, so the throw lands on this run's guard, not the task's try.
  #export = "nested()"
  static nested() {
    Fiber.spawn {
      innerResult = Player.withTry(Fn.new { Fiber.sleep(2) })
      innerDone.release()
    }
    Fiber.tick(0)
    Player.explodeNow()
  }

  #export = "finish() -> String"
  static finish() {
    innerDone.wait()
    return innerResult
  }

  // Two Wren tasks, each in a Haxe call that parks inside a Haxe try,
  // their parks interleaved: what each try catches after its park is what
  // Haxe threw there, so each task's traps are its own.
  #export = "guarded() -> String"
  static guarded() {
    var done = Lock.new()
    var results = []
    for (ms in [2, 4]) {
      Fiber.spawn {
        results.add(Player.napThenThrow(ms))
        done.release()
      }
    }
    done.wait()
    done.wait()
    return results.join(" ")
  }
}
