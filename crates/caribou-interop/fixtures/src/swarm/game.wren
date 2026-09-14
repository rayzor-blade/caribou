// The arena's gameplay, in Wren: each entity's behaviour, the collision
// rule, the waves, a tween the engine drives, and the HUD. The engine
// (swarm/World.hx) keeps the world and calls in here every frame; this
// side reads and writes the world through World's statics and an entity's
// own fields. The same behaviour is written in Haxe (Swarm.hx) and in
// Wren over a Wren world (arena.wren), for the baselines.
import "swarm:World" for World

class Boid {
  #export = "new(e, i: Num)"
  construct new(e, i) {
    _e = e
    _i = i
  }

  // Flocking: toward the neighbours' centre, along their heading, away
  // from the close ones.
  #export = "update(dt: Num) -> Num"
  update(dt) {
    var e = _e
    var i = _i
    var x = e.x
    var y = e.y
    var vx = e.vx
    var vy = e.vy
    var n = World.neighborCount(i)
    var cx = 0
    var cy = 0
    var avx = 0
    var avy = 0
    var sx = 0
    var sy = 0
    var k = 0
    while (k < n) {
      var j = World.neighborAt(i, k)
      var dx = World.x(j) - x
      var dy = World.y(j) - y
      cx = cx + dx
      cy = cy + dy
      avx = avx + World.vx(j)
      avy = avy + World.vy(j)
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
      e.vx = vx
      e.vy = vy
    }
    return vx + vy
  }
}

class Game {
  #export = "reset() -> Null"
  static reset() {
    __score = 0
  }

  #export = "score() -> Num"
  static score() { __score }

  #export = "onCollide(a: Num, b: Num) -> Null"
  static onCollide(a, b) {
    World.bounce(a, b)
    __score = __score + 1
  }

  // How many to spawn this frame: a wave every thirty.
  #export = "spawnCount(frame: Num) -> Num"
  static spawnCount(frame) { frame % 30 == 0 ? 2 : 0 }

  // A curve the engine samples every frame until the next wave.
  #export = "waveTween(frame: Num) -> Fn(Num) -> Num"
  static waveTween(frame) {
    var scale = frame % 7 + 1
    return Fn.new {|t| t * scale }
  }

  #export = "hud() -> String"
  static hud() { "score %(__score)" }
}
