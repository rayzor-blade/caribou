import window.Samples;
import window.Event;
import window.ScaleSizing;
import window.Theme;

class UseWindowEvents {
    static function check(ok:Bool, message:String) {
        if (!ok) throw message;
    }
    static function main() {
        // The events a window reports, made by the plugin's generated model,
        // kept across a collection before they are read.
        function event(index:Int):Event {
            var e = Samples.event(index);
            hl.Gc.major();
            return e;
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
        switch (Samples.sizing(0)) { case Logical: default: throw "logical sizing"; }
        switch (Samples.sizing(1)) { case Physical: default: throw "physical sizing"; }
        check(Samples.theme(Dark) == 1 && Samples.theme(null) == -1, "optional theme");
        Sys.println("window events ok");
    }
}
