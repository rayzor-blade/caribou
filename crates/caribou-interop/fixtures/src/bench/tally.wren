// The interop benchmark's Wren side: the same loops as Bench.hx, the
// `wren*` ones the baseline inside Wren and the `haxe*` ones against the
// Haxe class Bench.
import "bench:Bench" for Bench

class Tally {
  #export = "new(t: Num)"
  construct new(t) { _t = t }

  #export = "add(x: Num) -> Num"
  static add(x) { x + 1 }

  #export = "bump(x: Num) -> Num"
  bump(x) {
    _t = _t + x
    return _t
  }

  #export = "total -> Num"
  total { _t }
  #export = "total=(v: Num)"
  total=(v) { _t = v }

  #export = "adder() -> Fn(Num) -> Num"
  static adder() { Fn.new {|x| x + 1 } }

  // Baselines.

  static wrenStatic(n) {
    var s = 0
    for (i in 0...n) s = Tally.add(s)
    return s
  }

  static wrenMethod(n) {
    var t = Tally.new(0)
    for (i in 0...n) t.bump(1)
    return t.total
  }

  static wrenGetter(n) {
    var t = Tally.new(3)
    var s = 0
    for (i in 0...n) s = s + t.total
    return s
  }

  static wrenSetter(n) {
    var t = Tally.new(0)
    for (i in 0...n) t.total = i
    return t.total
  }

  static wrenClosure(n) {
    var f = Tally.adder()
    var s = 0
    for (i in 0...n) s = f.call(s)
    return s
  }

  static wrenNew(n) {
    var t = null
    for (i in 0...n) t = Tally.new(i)
    return t.total
  }

  // Into Haxe.

  static haxeStatic(n) {
    var s = 0
    for (i in 0...n) s = Bench.add(s)
    return s
  }

  static haxeMethod(n) {
    var b = Bench.new()
    for (i in 0...n) b.bump(1)
    return b.v
  }

  static haxeGetter(n) {
    var b = Bench.new()
    b.v = 3
    var s = 0
    for (i in 0...n) s = s + b.v
    return s
  }

  static haxeSetter(n) {
    var b = Bench.new()
    for (i in 0...n) b.v = i
    return b.v
  }

  static haxeClosure(n) {
    var f = Bench.adder()
    var s = 0
    for (i in 0...n) s = f.call(s)
    return s
  }

  static haxeNew(n) {
    var b = null
    for (i in 0...n) b = Bench.new()
    return b.v
  }
}
