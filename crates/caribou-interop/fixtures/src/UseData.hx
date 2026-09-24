import math.Data;
import math.Event;
import math.Nested;
import haxe.io.Bytes;

class UseData {
    static function check(ok:Bool, what:String) {
        if (!ok) throw what;
    }
    static function main() {
        // Neither returned class is imported or annotated. Their methods
        // must be reached through the types in the plugin signatures.
        var vector = Data.vector();
        check(vector.len() == 5, "inferred static return type");
        var tally = vector.tally();
        hl.Gc.major();
        check(tally.add(2) == 7, "inferred instance return type");
        var b:Bytes = Data.bytes();
        check(b.length == 4 && b.get(0) == 0 && b.get(1) == 128 && b.get(2) == 255, "binary bytes");
        var alias:Bytes = Data.echo(b);
        check(Data.same_storage(b, alias), "buffer storage was copied");
        alias.set(0, 23);
        check(b.get(0) == 23 && Data.sum(b) == 471, "shared mutation");
        var haxeBytes = Bytes.alloc(3);
        haxeBytes.set(0, 255);
        haxeBytes.set(2, 128);
        Data.save(haxeBytes);
        haxeBytes = null;
        hl.Gc.major();
        check(Data.saved().get(0) == 255 && Data.saved().get(2) == 128, "kept buffer");
        check(Data.empty().length == 0, "empty buffer");
        switch (Data.event(0)) {
            case Closed:
            default: throw "nullary constructor";
        }
        switch (Data.event(1)) {
            case Resized(w, h): check(w == 800 && h == 600, "payload");
            default: throw "wrong constructor";
        }
        check(Data.area(Resized(7, 9)) == 63, "Haxe enum to Rust");
        switch (Data.echo_event(Message("from Haxe", b, false, 2.5))) {
            case Message(label, bytes, enabled, ratio):
                check(label == "from Haxe" && !enabled && ratio == 2.5, "mixed payload");
                check(Data.same_storage(b, bytes), "enum buffer storage was copied");
            default: throw "wrong mixed constructor";
        }
        var message = Data.event(2);
        hl.Gc.major();
        switch (message) {
            case Message(label, bytes, enabled, ratio):
                check(label == "héllo" && bytes.get(1) == 255 && enabled && ratio == 1.5, "collected payload");
            default: throw "wrong message constructor";
        }
        switch (Data.nested(Resized(3, 4))) {
            case Event(Resized(w, h)): check(w * h == 12, "nested enum");
            default: throw "wrong nested constructor";
        }
        switch (Data.event(3)) {
            case Wide(n): check(n.high == 1 && n.low == 2, "64-bit enum field");
            default: throw "wrong wide constructor";
        }
        check(Data.area(Wide(haxe.Int64.make(2, 3))) == 2, "Haxe 64-bit payload");
        var future = Data.later(73);
        check(future.await() == 73, "future result");
        var delayedVector = Data.later_vec().await();
        check(delayedVector.len() == 10, "typed future resource result");
        var manual = new caribou.Future<Int>();
        check(!manual.ready(), "new future ready");
        check(manual.resolve(91) && !manual.resolve(92), "future settled twice");
        check(manual.await() == 91, "manual future result");
        var rejected = new caribou.Future<Int>();
        check(rejected.reject("future failed"), "future rejection");
        var rejectedCaught = false;
        try rejected.await() catch (error:Dynamic) rejectedCaught = true;
        check(rejectedCaught, "rejected future returned");
        var pair = Data.pair();
        hl.Gc.major();
        // Decode into ordinary Rust nested enums, then encode them again.
        switch (Data.rebuild_nested(pair)) {
            case Pair(Message(label, bytes, enabled, ratio), Wide(n)):
                check(label == "first" && bytes.get(0) == 42 && enabled && ratio == 2.5, "derived named fields");
                check(n.high == 1 && n.low == 2, "rooted nested enum");
                switch (pair) {
                    case Pair(Message(_, original, _, _), _):
                        check(Data.same_storage(bytes, original), "derived enum copied buffer");
                    default: throw "original pair";
                }
            default: throw "derived nested pair";
        }
        var caught = false;
        try { Data.area(cast "not an enum"); } catch (_:Dynamic) { caught = true; }
        check(caught, "bad enum accepted");
        caught = false;
        try { Data.sum(cast "not bytes"); } catch (_:Dynamic) { caught = true; }
        check(caught, "bad buffer accepted");
        Sys.println("data ok");
    }
}
