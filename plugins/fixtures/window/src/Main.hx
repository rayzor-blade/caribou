import window.Event;
import window.WindowBuilder;

class Main {
    static function main() {
        var window = new WindowBuilder()
            .title("Caribou Window Test")
            .size(800, 600)
            .open();


        var running = true;
        while (running) {
            switch (window.poll()) {
                case None:
                    Sys.sleep(0.001);
                case Resized(width, height):
                    Sys.println('Resized to ${width}x${height}');
                case Moved(x, y):
                    Sys.println('Moved to ${x}, ${y}');
                case CursorEntered:
                    Sys.println('Cursor entered window');
                case CursorLeft:
                    Sys.println('Cursor left window');
                case CursorMoved(x, y):
                    Sys.println('Cursor moved to ${x}, ${y}');
                case MouseInput(state, button):
                    Sys.println('Mouse input: ${state}, ${button}');
                case MouseWheel(delta, phase):
                    Sys.println('Mouse wheel: ${delta}, ${phase}');
                case Closed:
                    running = false;
            }
        }
        window.close();
    }
}
