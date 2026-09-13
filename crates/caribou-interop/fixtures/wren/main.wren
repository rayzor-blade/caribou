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
var q = Player.spawnAt(3, 4)
System.print(q.hp)
