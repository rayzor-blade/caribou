# Caribou Window Management Plugin
Rust winit based window management plugin for Caribou tunime. 

Example:

```haxe 

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
                ...
                case Closed | Destroyed:
                    running = false;
            }
        }
        window.close();
    }
}
```

## Window events

`events.rs` contains the plugin's event definitions and conversions. `events/keys.rs` lists all 194 `KeyCode` and 306 `NamedKey` variants in winit 0.30.13. `lib.rs` owns windows, pumps winit and exposes the plugin API.

All 28 `winit::event::WindowEvent` variants have an explicit mapping. The derive has no fallback: a new native window variant causes a compile error until its payload is handled. `None` is only the empty-queue sentinel. `CloseRequested` retains the plugin's existing name, `Closed`.

| Native events | Exported data |
| --- | --- |
| Resize, move, close, destroy, focus, occlusion, redraw, theme | `Resized`, `Moved`, `Closed`, `Destroyed`, `Focused`, `Occluded`, `RedrawRequested`, `ThemeChanged` |
| Cursor and mouse | Coordinates, button/state or scroll/phase, plus an interned `device_id` |
| Keyboard | `KeyboardInput(device_id, KeyEvent.Input(...), is_synthetic)` |
| Modifier changes | Effective shift/control/alt/super flags and all eight left/right key states |
| IME | Enabled, disabled, committed text, or preedit text with an optional byte-index cursor range |
| File drop/hover | `FilePath`: Unicode text or the exact native bytes for non-Unicode paths |
| Pinch, pan, double tap, rotation, pressure, axis | Original device, phase, coordinate and numeric payloads |
| Touch | Device, phase, coordinates, optional force calibration and all 64 touch-ID bits |
| Scale-factor change | Queued factor notification; synchronous size selection through the callback below |
| Activation token | Correlation ID and token string; `request_activation_token()` returns the same ID, or zero on unsupported platforms |

`Device(device_id, DeviceEvent)` additionally exposes all seven raw device event variants: added, removed, mouse motion, wheel, axis motion, button and raw keyboard input. Delivery follows winit's default device-event policy and platform support. `Resumed`, `Suspended` and `MemoryWarning` are forwarded from the corresponding application callbacks. Polling does not enqueue per-pump `AboutToWait` notifications, so an idle queue can return `None`.

## Payloads and ownership

Events stay ordinary Rust values in the queue. Polling encodes them into the shared Caribou heap and roots nested values during allocation. No JSON or other serialized event buffer is involved. Rust strings copy into the host once; native path bytes become a `Buffer` only when delivered.

- Device IDs are interned by winit identity into positive `Int` values, shared by raw and window event conversion on the event-loop thread. They are not hashes or casts of private native layouts. Winit may use different identities for a raw physical device and a window's virtual device.
- Resize dimensions, native 32-bit key/axis codes, IME byte offsets and activation-request IDs use `Int64` in Haxe, preserving their full ranges. Touch IDs preserve all unsigned 64 bits in the `Int64` bit pattern; a native `u64::MAX` is represented as `-1`.
- Wren receives an `Int64` as a `Num` when a double holds it exactly, and otherwise as an `Int64` instance that keeps every bit, so any touch ID round-trips. Wren builds enums through their classes: `ScaleSize.Physical(640, 480)`, `Event.RedrawRequested`. The existing Zyntax adapter gap is `c575125`.
- Existing mouse and cursor variants now include a device ID. Rebuild Haxe bytecode against the updated plugin; the fixture shows the new patterns.
- `KeyEvent.Input` includes physical key, logical key, optional text, location, state, repeat and platform modifier supplements. `OptionalText.None` is distinct from an empty string. The existing `MouseElementState` (`Pressed`/`Released`) also represents keyboard state.
- Physical and named keys are actual enums. Winit marks these two source enums non-exhaustive; `Unrecognized` is reserved for future keys and the lists should be updated when upgrading winit. Native unidentified platform key codes remain available separately.
- `TouchForce.None`, `OptionalFloat.None`, `OptionalText.None` and `CursorRange.None` preserve missing values. No zero or empty-string sentinel replaces them.
- `FilePath.Utf8` contains a valid Unicode path. `UnixBytes` contains the exact native byte sequence. `WindowsWide` contains UTF-16 code units encoded as little-endian bytes. The path is never silently replaced by lossy Unicode on supported desktop targets.

Event types describe winit's complete data model; platforms still emit only the events they support. The plugin's event-loop implementation uses winit's `pump_events` API, so it is limited to that API's supported targets.

## Scale changes and event controls

Winit's `InnerSizeWriter` is valid only during its native scale-change callback. It is not stored in an event or exposed as an expired handle. Install a synchronous callback to override the proposed physical size:

```haxe
window.on_scale_factor_changed(function(factor:Float):window.ScaleSize {
    return Physical(Std.int(800 * factor), Std.int(600 * factor));
});
```

Return `Default` to preserve winit's suggestion, or pass `null` to `on_scale_factor_changed` to remove the callback. The callback is retained by a GC handle and released when replaced or when the window closes. Negative dimensions and wrong return types are errors. Callback exceptions reach the `poll()` caller, while the queued notification remains available on the next poll.

The callback runs within `poll()`'s event pump. It must return the size without calling window operations recursively; such re-entry reports a runtime error instead of panicking on a `RefCell` borrow. Cache any other window information before polling. Wren callers can return `WindowHandle.physical_scale_size(width, height)` or `WindowHandle.default_scale_size()` from their callback.

`request_redraw()` requests a redraw event. `set_ime_allowed(bool)` enables IME input and `set_ime_cursor_area(x, y, width, height)` supplies its logical cursor rectangle. `set_size(width, height)` requests the logical inner size; it does not change the minimum-size constraint. `request_activation_token()` is supported on the X11/Wayland targets that provide startup notification.

## Checks

`cargo test -p caribou-window --offline` tests native conversions without opening a window. The interop tests `plugin_window_events` and `plugin_window_events_wren` use the production schemas in a separate test plugin, checking nested payloads, identity, integer widths, path bytes and synchronous callback success/error cases. They also run with `ASH_GC_STRESS=1 WLIFT_GC_STRESS=1`.

The interactive example is `plugins/fixtures/window`. The headless Haxe fixture is `crates/caribou-interop/fixtures/window_events/events.hxml`; its plugin library is built by the interop build script and its bytecode is checked in.

Nested Wren event payloads exposed an interpreter bug when lazy class installation moved the module table during an expression. The fix is in the main sibling wrenlift repository, referenced by the workspace Cargo and tracked in Wren's git-bug `eb28d58`.
