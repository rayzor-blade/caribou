// The flock's gameplay: where each bird is, where it heads, and what the
// engine is told. The engine (Main.hx) draws it and reports input.
import "palette" for Palette

class Flock {
  #export = "new(count: Num, width: Num, height: Num)"
  construct new(count, width, height) {
    _width = width
    _height = height
    // Each bird as the engine draws it, four floats: x, y, heading, hue.
    _birds = Float32Array.new(Flock.limit * 4)
    _vx = Float32Array.new(Flock.limit)
    _vy = Float32Array.new(Flock.limit)
    // The neighbour grid: the first bird in each cell, and the next bird
    // in the same cell after each bird.
    _head = Int32Array.new(0)
    _next = Int32Array.new(Flock.limit)
    _count = 0
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
    _birds[_count * 4] = x
    _birds[_count * 4 + 1] = y
    _vx[_count] = random_ * 200 - 100
    _vy[_count] = random_ * 200 - 100
    _count = _count + 1
  }

  #export = "resize(width: Num, height: Num) -> Null"
  resize(width, height) {
    _width = width
    _height = height
  }

  // A burst of birds where the player clicked; a full flock refuses.
  #export = "spawn(x: Num, y: Num) -> Null"
  spawn(x, y) {
    if (_count >= Flock.limit) Fiber.abort("the flock is full")
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
    if (_head.count != columns * rows) _head = Int32Array.new(columns * rows)
    for (cell in 0..._head.count) _head[cell] = -1
    var birds = _birds
    for (i in 0..._count) {
      var cell = (birds[i * 4] / reach).floor + (birds[i * 4 + 1] / reach).floor * columns
      _next[i] = _head[cell]
      _head[cell] = i
    }
    for (i in 0..._count) {
      var x = birds[i * 4]
      var y = birds[i * 4 + 1]
      var column = (x / reach).floor
      var row = (y / reach).floor
      var seen = 0
      var cx = 0
      var cy = 0
      var avx = 0
      var avy = 0
      var sx = 0
      var sy = 0
      var lastRow = (row + 1).min(rows - 1)
      var firstColumn = (column - 1).max(0)
      var lastColumn = (column + 1).min(columns - 1)
      var r = (row - 1).max(0)
      while (r <= lastRow) {
        var c = firstColumn
        while (c <= lastColumn) {
          var j = _head[c + r * columns]
          while (j >= 0 && seen < heeded) {
            if (j != i) {
              var dx = x - birds[j * 4]
              var dy = y - birds[j * 4 + 1]
              var d2 = dx * dx + dy * dy
              if (d2 < reach * reach) {
                seen = seen + 1
                cx = cx + birds[j * 4]
                cy = cy + birds[j * 4 + 1]
                avx = avx + _vx[j]
                avy = avy + _vy[j]
                if (d2 < near * near) {
                  var d = d2.sqrt + 0.01
                  sx = sx + dx / d * (near - d)
                  sy = sy + dy / d * (near - d)
                }
              }
            }
            j = _next[j]
          }
          c = c + 1
        }
        r = r + 1
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
      vx = vx / speed * bounded
      vy = vy / speed * bounded
      _vx[i] = vx
      _vy[i] = vy
      x = (x + vx * dt) % _width
      y = (y + vy * dt) % _height
      if (x < 0) x = x + _width
      if (y < 0) y = y + _height
      birds[i * 4] = x
      birds[i * 4 + 1] = y
      birds[i * 4 + 2] = vy.atan(vx)
      birds[i * 4 + 3] = Palette.hue(i, _time)
    }
  }

  #export = "count -> Num"
  count { _count }

  // The birds as the engine draws them, the first `count` of them in use:
  // the array itself, which the engine reads where it lies.
  #export = "birds -> Float32Array"
  birds { _birds }

  #export = "hud -> String"
  hud { "%(_count) birds at %((_time * 10).floor / 10)s" }
}
