// The Wren side of the Haxe-imports-Wren test: on the classpath at
// game/hud.wren, so Haxe code reaches it as `game.hud.Hud`. It imports a
// class of the Haxe program the same way.
import "game:Player" for Player

class Hud {
  construct new(score) { _score = score }

  #export = "add(n: Num) -> Num"
  add(n) {
    _score = _score + n
    return _score
  }

  score { _score }
  score=(v) { _score = v }

  // The result is inferred: an interpolation is a String.
  #export = "label(prefix: String)"
  label(prefix) { "%(prefix): %(_score)" }

  // Exported under another name.
  #export = "explode()"
  fail() { Fiber.abort("boom") }

  // Inferred too: a constructor call is the class.
  static make(score) { Hud.new(score) }

  #export = "best(a: Hud, b: Hud) -> Hud"
  static best(a, b) { a.score > b.score ? a : b }

  // A Haxe object made and read here: its name, a Haxe String.
  owner() { Player.new("ada").name }
}
