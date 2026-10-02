import "cyc:py_side" for PySide

class WrenSide {
  static double(x) { x * 2 }
  static make(n) { PySide.new(n) }
}
