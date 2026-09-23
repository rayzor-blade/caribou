import window.Samples;
import window.Event;
import window.ScaleSize;

class UseWindowEvents {
    static function check(ok:Bool, message:String) {
        if (!ok) throw message;
    }
    static function main() {
        // Decode into the production Rust event types and encode again.
        function event(index:Int):Event {
            var e = Samples.event(index);
            hl.Gc.major();
            return Samples.echo(e);
        }
        switch (event(0)) {
            case Resized(width, height):
                check(width.high == 0 && width.low == -1 && height.low == 600, "u32 dimensions truncated");
            default: throw "resize";
        }
        switch (event(1)) {
            case Ime(Preedit(text, Range(start, end))):
                check(text == "é文" && start.low == 2 && end.low == 5, "IME byte range");
            default: throw "IME";
        }
        var device = 0;
        switch (event(2)) {
            case Touch(d, Moved, x, y, Calibrated(force, maxForce, Some(angle)), id):
                device = d;
                check(x == 1.5 && y == -2.5 && force == 2 && maxForce == 4 && angle == 0.5, "touch payload");
                check(id.high == -1 && id.low == -1, "touch ID bits");
            default: throw "touch";
        }
        switch (event(3)) {
            case KeyboardInput(d, Input(Code(KeyA), Character(key), Some(text), Left, Pressed, repeat, Supplement(Named(Enter), None)), synthetic):
                check(d == device && key == "é" && text == "é" && repeat && synthetic, "keyboard fields");
            default: throw "keyboard";
        }
        switch (event(4)) {
            case MouseInput(Released, Other(button), d): check(button == 65535 && d == device, "mouse identity");
            default: throw "mouse";
        }
        switch (event(5)) {
            case Device(d, Key(Unidentified(Xkb(code)), Pressed)):
                check(d == device && code.high == 0 && code.low == -1, "native key code");
            default: throw "raw key";
        }
        switch (event(6)) {
            case DroppedFile(UnixBytes(bytes)): check(bytes.length == 2 && bytes.get(1) == 255, "non-Unicode path");
            default: throw "file";
        }
        switch (event(7)) { case RedrawRequested: default: throw "redraw"; }
        switch (event(8)) {
            case ModifiersChanged(State(shift, control, alt, superKey, _, _, _, _, _, _, _, _)):
                check(shift && !control && !alt && !superKey, "modifiers");
            default: throw "modifiers";
        }
        switch (event(9)) { case ThemeChanged(Dark): default: throw "theme"; }
        switch (Samples.scale(function(factor:Float):ScaleSize {
            hl.Gc.major();
            check(factor == 1.25, "scale argument");
            return Physical(960, 720);
        }, 1.25)) {
            case Physical(w, h): check(w == 960 && h == 720, "scale result");
            default: throw "scale request";
        }
        switch (Samples.scale(function(_:Float):ScaleSize { return Default; }, 2)) {
            case Default:
            default: throw "default scale size";
        }
        var caught = false;
        try { Samples.scale(function(_:Float):ScaleSize { throw "scale failure"; }, 1); }
        catch (e:Dynamic) { caught = Std.string(e).indexOf("scale failure") >= 0; }
        check(caught, "callback error was swallowed");
        caught = false;
        try { Samples.scale(function(_:Float):ScaleSize { return Physical(-1, 20); }, 1); }
        catch (_:Dynamic) { caught = true; }
        check(caught, "negative scale dimensions accepted");
        caught = false;
        try { Samples.scale(function(_:Float):Dynamic { return "wrong"; }, 1); }
        catch (_:Dynamic) { caught = true; }
        check(caught, "wrong scale callback result accepted");
        Sys.println("window events ok");
    }
}
