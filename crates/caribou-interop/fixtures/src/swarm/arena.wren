// The whole arena in Wren: the engine of swarm/World.hx and the behaviour
// of game.wren, over lists, for the all-Wren baseline. Same arithmetic in
// the same order, so the checksum is the same.

class WorldW {
  construct new(seed) {
    _seed = seed
    _x = []
    _y = []
    _vx = []
    _vy = []
    _nb = []
    _nbCount = []
    _next = []
    _head = List.filled(400, -1)
    _collA = []
    _collB = []
    _collN = 0
  }

  count { _x.count }
  x(i) { _x[i] }
  y(i) { _y[i] }
  vx(i) { _vx[i] }
  vy(i) { _vy[i] }
  setV(i, vx, vy) {
    _vx[i] = vx
    _vy[i] = vy
  }
  neighborCount(i) { _nbCount[i] }
  neighborAt(i, k) { _nb[i * 8 + k] }
  collisions { _collN }
  collisionA(k) { _collA[k] }
  collisionB(k) { _collB[k] }

  random() {
    _seed = (_seed * 16807) % 2147483647
    return _seed / 2147483647
  }

  spawn(x, y, vx, vy) {
    _x.add(x)
    _y.add(y)
    _vx.add(vx)
    _vy.add(vy)
    var k = 0
    while (k < 8) {
      _nb.add(-1)
      k = k + 1
    }
    _nbCount.add(0)
    _next.add(-1)
    return _x.count - 1
  }

  spawnRandom() {
    var x = random() * 1000
    var y = random() * 1000
    var vx = (random() - 0.5) * 60
    var vy = (random() - 0.5) * 60
    return spawn(x, y, vx, vy)
  }

  bounce(a, b) {
    var vx = _vx[a]
    var vy = _vy[a]
    _vx[a] = _vx[b]
    _vy[a] = _vy[b]
    _vx[b] = vx
    _vy[b] = vy
  }

  step(dt) {
    var n = _x.count
    var i = 0
    while (i < n) {
      var x = _x[i] + _vx[i] * dt
      var y = _y[i] + _vy[i] * dt
      if (x < 0) x = x + 1000
      if (x >= 1000) x = x - 1000
      if (y < 0) y = y + 1000
      if (y >= 1000) y = y - 1000
      _x[i] = x
      _y[i] = y
      i = i + 1
    }
    var c = 0
    while (c < 400) {
      _head[c] = -1
      c = c + 1
    }
    i = n - 1
    while (i >= 0) {
      var cell = (_y[i] / 50).floor * 20 + (_x[i] / 50).floor
      _next[i] = _head[cell]
      _head[cell] = i
      i = i - 1
    }
    i = 0
    while (i < n) {
      var cx = (_x[i] / 50).floor
      var cy = (_y[i] / 50).floor
      var found = 0
      var dy = -1
      while (dy <= 1 && found < 8) {
        var dx = -1
        while (dx <= 1 && found < 8) {
          var cell = ((cy + dy + 20) % 20) * 20 + ((cx + dx + 20) % 20)
          var j = _head[cell]
          while (j >= 0 && found < 8) {
            if (j != i) {
              _nb[i * 8 + found] = j
              found = found + 1
            }
            j = _next[j]
          }
          dx = dx + 1
        }
        dy = dy + 1
      }
      _nbCount[i] = found
      i = i + 1
    }
    _collN = 0
    c = 0
    while (c < 400) {
      var a = _head[c]
      while (a >= 0) {
        var b = _next[a]
        while (b >= 0) {
          var dx = _x[a] - _x[b]
          var dy = _y[a] - _y[b]
          if (dx * dx + dy * dy < 64) {
            if (_collN < _collA.count) {
              _collA[_collN] = a
              _collB[_collN] = b
            } else {
              _collA.add(a)
              _collB.add(b)
            }
            _collN = _collN + 1
          }
          b = _next[b]
        }
        a = _next[a]
      }
      c = c + 1
    }
  }

  checksum() {
    var s = 0
    var i = 0
    var n = _x.count
    while (i < n) {
      s = s + (_x[i] + _y[i] + _vx[i] + _vy[i])
      i = i + 1
    }
    return s
  }
}

class BoidW {
  construct new(world, i) {
    _w = world
    _i = i
  }

  update(dt) {
    var w = _w
    var i = _i
    var x = w.x(i)
    var y = w.y(i)
    var vx = w.vx(i)
    var vy = w.vy(i)
    var n = w.neighborCount(i)
    var cx = 0
    var cy = 0
    var avx = 0
    var avy = 0
    var sx = 0
    var sy = 0
    var k = 0
    while (k < n) {
      var j = w.neighborAt(i, k)
      var dx = w.x(j) - x
      var dy = w.y(j) - y
      cx = cx + dx
      cy = cy + dy
      avx = avx + w.vx(j)
      avy = avy + w.vy(j)
      if (dx * dx + dy * dy < 100) {
        sx = sx - dx
        sy = sy - dy
      }
      k = k + 1
    }
    if (n > 0) {
      var ax = cx / n * 0.02 + (avx / n - vx) * 0.05 + sx * 0.1
      var ay = cy / n * 0.02 + (avy / n - vy) * 0.05 + sy * 0.1
      vx = vx + ax * dt * 60
      vy = vy + ay * dt * 60
      if (vx > 60) vx = 60
      if (vx < -60) vx = -60
      if (vy > 60) vy = 60
      if (vy < -60) vy = -60
      w.setV(i, vx, vy)
    }
    return vx + vy
  }
}

class Arena {
  #export = "run(n: Num, frames: Num) -> Num"
  static run(n, frames) {
    var w = WorldW.new(12345)
    var score = 0
    var boids = []
    var i = 0
    while (i < n) {
      boids.add(BoidW.new(w, w.spawnRandom()))
      i = i + 1
    }
    var dt = 1 / 60
    var acc = 0
    var tween = null
    var hud = 0
    var frame = 0
    while (frame < frames) {
      var b = 0
      var count = boids.count
      while (b < count) {
        acc = acc + boids[b].update(dt)
        b = b + 1
      }
      w.step(dt)
      var c = w.collisions
      var k = 0
      while (k < c) {
        w.bounce(w.collisionA(k), w.collisionB(k))
        score = score + 1
        k = k + 1
      }
      var spawn = frame % 30 == 0 ? 2 : 0
      var s = 0
      while (s < spawn) {
        boids.add(BoidW.new(w, w.spawnRandom()))
        s = s + 1
      }
      if (frame % 60 == 0) {
        var scale = frame % 7 + 1
        tween = Fn.new {|t| t * scale }
        hud = hud + "score %(score)".count
      }
      if (tween != null) acc = acc + tween.call(frame * dt)
      frame = frame + 1
    }
    return w.checksum() + acc + score + hud
  }
}
