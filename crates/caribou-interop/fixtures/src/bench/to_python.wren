// The interop benchmark's Wren→Python column: the same loops as tally.wren,
// against the Python class PythonTally. A module of its own because
// python_tally imports tally, and an import back would be a cycle.
import "bench:python_tally" for PythonTally

class ToPython {
  #export = "pythonStatic(n: Num) -> Num"
  static pythonStatic(n) {
    var s = 0
    for (i in 0...n) s = PythonTally.add(s)
    return s
  }

  #export = "pythonMethod(n: Num) -> Num"
  static pythonMethod(n) {
    var t = PythonTally.new(0)
    for (i in 0...n) t.bump(1)
    return t.value
  }

  #export = "pythonGetter(n: Num) -> Num"
  static pythonGetter(n) {
    var t = PythonTally.new(3)
    var s = 0
    for (i in 0...n) s = s + t.value
    return s
  }

  #export = "pythonSetter(n: Num) -> Num"
  static pythonSetter(n) {
    var t = PythonTally.new(0)
    for (i in 0...n) t.value = i
    return t.value
  }

  #export = "pythonClosure(n: Num) -> Num"
  static pythonClosure(n) {
    var f = PythonTally.adder()
    var s = 0
    for (i in 0...n) s = f.call(s)
    return s
  }

  #export = "pythonNew(n: Num) -> Num"
  static pythonNew(n) {
    var t = null
    for (i in 0...n) t = PythonTally.new(i)
    return t.value
  }
}
