// The Wren side of the watch test: on the classpath at game/tune.wren,
// reached from Haxe as `game.tune.Tune`. The test edits a copy of it
// while the program runs.
class Tune {
  #export = "value() -> Num"
  static value() { 1 }
}
