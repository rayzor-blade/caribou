// The Wren side of the one-world test: on the classpath at
// game/relay.wren, reached from Haxe as `game.relay.Relay`. Its fibers
// and threads are tasks of the world Haxe's threads run on. The state is
// in module variables: a closure in a static method does not see the
// class's static fields (wren_lift issue 683f3dd).
import "thread" for Thread, Lock
import "game:Player" for Player

var log = []
var lock = Lock.new()

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
