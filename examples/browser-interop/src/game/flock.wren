// The flock's gameplay: where each bird is, where it heads, and what the
// engine is told. The engine (Main.hx) draws it and reports input.
import "palette" for Palette

class Flock {
  #export = "new(count: Num, width: Num, height: Num)"
  construct new(count, width, height) {
    _width = width
    _height = height
    _x = []
    _y = []
    _vx = []
    _vy = []
    _seed = 7
    _time = 0
    _targetX = width / 2
    _targetY = height / 2
    for (i in 0...count) add_(random_ * width, random_ * height)
  }

  // Park-Miller: the same flock on every run.
  random_ {
    _seed = (_seed * 16807) % 2147483647
    return _seed / 2147483647
  }

  add_(x, y) {
    _x.add(x)
    _y.add(y)
    _vx.add(random_ * 200 - 100)
    _vy.add(random_ * 200 - 100)
  }

  #export = "resize(width: Num, height: Num) -> Null"
  resize(width, height) {
    _width = width
    _height = height
  }

  // A burst of birds where the player clicked; a full flock refuses.
  #export = "spawn(x: Num, y: Num) -> Null"
  spawn(x, y) {
    if (_x.count >= Flock.limit) Fiber.abort("the flock is full")
    for (i in 0...25) add_(x, y)
    _targetX = x
    _targetY = y
  }

  #export = "limit -> Num"
  static limit { 2000 }

  // Boids: each bird steers away from birds too close, toward the heading
  // and centre of its neighbours, and a little toward a target that
  // wanders; its speed stays within bounds, and it wraps at the edges.
  // Neighbours come from a grid of cells one neighbourhood wide, and a
  // bird heeds the first seven it finds, as a starling heeds about seven.
  #export = "step(dt: Num) -> Null"
  step(dt) {
    _time = _time + dt
    var tx = _targetX + (_time * 0.7).cos * _width * 0.3
    var ty = _targetY + (_time * 1.1).sin * _height * 0.3
    var reach = 40
    var near = 14
    var heeded = 7
    var columns = (_width / reach).ceil + 1
    var rows = (_height / reach).ceil + 1
    var head = List.filled(columns * rows, -1)
    var next = List.filled(_x.count, -1)
    for (i in 0..._x.count) {
      var cell = (_x[i] / reach).floor + (_y[i] / reach).floor * columns
      next[i] = head[cell]
      head[cell] = i
    }
    for (i in 0..._x.count) {
      var x = _x[i]
      var y = _y[i]
      var column = (x / reach).floor
      var row = (y / reach).floor
      var seen = 0
      var cx = 0
      var cy = 0
      var avx = 0
      var avy = 0
      var sx = 0
      var sy = 0
      for (r in (row - 1).max(0)..(row + 1).min(rows - 1)) {
        for (c in (column - 1).max(0)..(column + 1).min(columns - 1)) {
          var j = head[c + r * columns]
          while (j >= 0 && seen < heeded) {
            if (j != i) {
              var dx = x - _x[j]
              var dy = y - _y[j]
              var d2 = dx * dx + dy * dy
              if (d2 < reach * reach) {
                seen = seen + 1
                cx = cx + _x[j]
                cy = cy + _y[j]
                avx = avx + _vx[j]
                avy = avy + _vy[j]
                if (d2 < near * near) {
                  var d = d2.sqrt + 0.01
                  sx = sx + dx / d * (near - d)
                  sy = sy + dy / d * (near - d)
                }
              }
            }
            j = next[j]
          }
        }
      }
      var ax = sx * 60
      var ay = sy * 60
      if (seen > 0) {
        ax = ax + (cx / seen - x) * 1.2 + (avx / seen - _vx[i]) * 1.5
        ay = ay + (cy / seen - y) * 1.2 + (avy / seen - _vy[i]) * 1.5
      }
      var dx = tx - x
      var dy = ty - y
      var d = (dx * dx + dy * dy).sqrt + 1
      ax = ax + dx / d * 40
      ay = ay + dy / d * 40
      var vx = _vx[i] + ax * dt
      var vy = _vy[i] + ay * dt
      var speed = (vx * vx + vy * vy).sqrt + 0.01
      var bounded = speed.clamp(70, 170)
      _vx[i] = vx / speed * bounded
      _vy[i] = vy / speed * bounded
      _x[i] = (x + _vx[i] * dt) % _width
      _y[i] = (y + _vy[i] * dt) % _height
      if (_x[i] < 0) _x[i] = _x[i] + _width
      if (_y[i] < 0) _y[i] = _y[i] + _height
    }
  }

  #export = "count -> Num"
  count { _x.count }

  #export = "x(i: Num) -> Num"
  x(i) { _x[i] }

  #export = "y(i: Num) -> Num"
  y(i) { _y[i] }

  #export = "heading(i: Num) -> Num"
  heading(i) { _vy[i].atan(_vx[i]) }

  #export = "hue(i: Num) -> Num"
  hue(i) { Palette.hue(i, _time) }

  #export = "hud -> String"
  hud { "%(_x.count) birds at %((_time * 10).floor / 10)s" }
}
