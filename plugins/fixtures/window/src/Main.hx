import window.Event;
import window.WindowBuilder;
import window.WindowHandle;

class Main {
    static function main() {
        var window: WindowHandle = new WindowBuilder()
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
                case Closed:
                    running = false;
            }
        }
        window.close();
    }
}
