// The Wren side of the imports test: a program that uses game.Player as if
// it were a Wren class. "game" is a namespace the world configures over the
// Haxe language; nothing here knows or says that Player is Haxe.
import "game:Player" for Player

var p = Player.new("ada")
System.print(p.name)
System.print(p.hit(30))
System.print(p.hp)
p.hp = 5
System.print(p.hit(10))
// The class's own static field, read and written where Haxe keeps it.
Player.spawned = 0
var q = Player.spawnAt(3, 4)
System.print(q.hp)
System.print(Player.spawned)
Player.spawned = 10
Player.spawnAt(0, 0)
System.print(Player.spawned)
// A Wren function where Haxe declares a typed callback, and an untyped one.
System.print(Player.twice(Fn.new {|x| x * 3 }, 2))
System.print(Player.apply(Fn.new {|s| s + "!" }, "hi"))
// One Haxe keeps and fires later, from its own code.
var onHit = Fn.new {|d| System.print("hit for %(d)") }
Player.onHit = onHit
p.hit(1)
// Read back from Haxe, it is the same function.
System.print(Player.onHit == onHit)
System.print(Player.onHit.arity)
// The callback is this module's; Haxe must not fire it once the module's
// VM is gone.
Player.onHit = null
