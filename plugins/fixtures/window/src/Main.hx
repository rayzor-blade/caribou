import window.WindowBuilder;
import window.WindowHandle;

class Main {
    static function main() {
        var window: WindowHandle = new WindowBuilder()
            .title("Caribou Window Test")
            .size(800, 600)
            .open();


        while (true) {
            var event = window.poll();
            Sys.println(event);

            if (event == 1) {
                break;
            }
        }
    }
}

