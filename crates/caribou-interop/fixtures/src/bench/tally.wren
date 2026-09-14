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

  // Objects, strings and sequences crossing (benches/transfer.rs).

  #export = "take(o: Bench) -> Num"
  static take(o) {
    __last = o
    return 0
  }
  #export = "make() -> Tally"
  static make() { Tally.new(0) }
  #export = "echo(s: String) -> String"
  static echo(s) { s }
  #export = "sum(xs: List) -> Num"
  static sum(xs) {
    var t = 0
    for (x in xs) t = t + x
    return t
  }

  static haxeSame(n) {
    var t = Tally.new(0)
    var s = 0
    for (i in 0...n) s = s + Bench.keep(t)
    return s
  }

  static haxeFresh(n) {
    var s = 0
    for (i in 0...n) s = s + Bench.keep(Tally.new(i))
    return s
  }

  static haxeReturned(n) {
    var b = null
    for (i in 0...n) b = Bench.make()
    return b.v
  }

  static haxeString(n) {
    var s = "hello"
    var len = 0
    for (i in 0...n) len = len + Bench.echo(s).count
    return len
  }

  static haxeSequence(n) {
    var xs = (0...100).toList
    var t = 0
    for (i in 0...(n / 100).floor) t = t + Bench.sum(xs)
    return t
  }

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
