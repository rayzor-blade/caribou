// A Wren system: counts its steps in byte 0, and writes the step's time
// as an f32 at byte 4.
import "game:Systems" for Systems

class SystemWren {
  static install() {
    Systems.register(Fn.new {|column, dt|
      column.setU8(0, column.getU8(0) + 1)
      column.setF32(4, dt)
    })
  }
}
