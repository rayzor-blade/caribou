import window.Event;
import window.WindowBuilder;
import window.ScaleSize;

class Main {
    static function main() {
        var window = new WindowBuilder()
            .title("Caribou Window Test")
            .size(800, 600)
            .open();
        window.set_ime_allowed(true);
        window.set_ime_cursor_area(20, 20, 400, 24);
        // This runs inside winit's scale callback. Return Physical(w, h)
        // to override its suggestion; window methods cannot re-enter it.
        window.on_scale_factor_changed(function(factor:Float):ScaleSize {
            return Default;
        });
        window.request_redraw();

        var running = true;
        while (running) {
            switch (window.poll()) {
                case None:
                    Sys.sleep(0.001);
                case Resized(width, height):
                    Sys.println('Resized to ${width}x${height}');
                case Moved(x, y):
                    Sys.println('Moved to ${x}, ${y}');
                case CursorEntered(device):
                    Sys.println('Cursor ${device} entered window');
                case CursorLeft(device):
                    Sys.println('Cursor ${device} left window');
                case CursorMoved(x, y, device):
                    Sys.println('Cursor ${device} moved to ${x}, ${y}');
                case MouseInput(state, button, device):
                    Sys.println('Mouse ${device}: ${state}, ${button}');
                case MouseWheel(delta, phase, device):
                    Sys.println('Mouse wheel ${device}: ${delta}, ${phase}');
                case KeyboardInput(device, Input(physical, logical, text, location, state, repeat, _), synthetic):
                    Sys.println('Key ${device}: ${physical}, ${logical}, ${text}, ${location}, ${state}, repeat=${repeat}, synthetic=${synthetic}');
                case ModifiersChanged(modifiers):
                    Sys.println('Modifiers: ${modifiers}');
                case Ime(event):
                    Sys.println('IME: ${event}');
                case DroppedFile(path):
                    Sys.println('Dropped: ${path}');
                case HoveredFile(path):
                    Sys.println('Hovered: ${path}');
                case HoveredFileCancelled:
                    Sys.println('File hover cancelled');
                case Focused(focused):
                    Sys.println('Focused: ${focused}');
                case PinchGesture(device, delta, phase):
                    Sys.println('Pinch ${device}: ${delta}, ${phase}');
                case PanGesture(device, x, y, phase):
                    Sys.println('Pan ${device}: ${x}, ${y}, ${phase}');
                case DoubleTapGesture(device):
                    Sys.println('Double tap ${device}');
                case RotationGesture(device, delta, phase):
                    Sys.println('Rotation ${device}: ${delta}, ${phase}');
                case TouchpadPressure(device, pressure, stage):
                    Sys.println('Pressure ${device}: ${pressure}, ${stage}');
                case AxisMotion(device, axis, value):
                    Sys.println('Axis ${device}/${axis}: ${value}');
                case Touch(device, phase, x, y, force, id):
                    Sys.println('Touch ${device}/${id}: ${phase}, ${x}, ${y}, ${force}');
                case ScaleFactorChanged(factor):
                    Sys.println('Scale: ${factor}');
                case ThemeChanged(theme):
                    Sys.println('Theme: ${theme}');
                case Occluded(occluded):
                    Sys.println('Occluded: ${occluded}');
                case ActivationTokenDone(serial, _):
                    Sys.println('Activation token received for request ${serial}');
                case RedrawRequested:
                    // Render here when this fixture has a renderer.
                case Device(device, event):
                    Sys.println('Device ${device}: ${event}');
                case Resumed:
                    Sys.println('Resumed');
                case Suspended:
                    Sys.println('Suspended');
                case MemoryWarning:
                    Sys.println('Memory warning');
                case Closed | Destroyed:
                    running = false;
            }
        }
        window.close();
    }
}
