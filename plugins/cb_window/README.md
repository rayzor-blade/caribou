# Window plugin

`caribou-window` is xwindow's Caribou adapter. It gives Haxe, Wren and
Zyntax programs the `window` API: windows, their events, monitors, cursors
and text input.

[xwindow](https://github.com/rayzor-blade/xwindow) owns the API
declaration, the generators, the winit backend and the page agent. This
crate supplies Caribou's carriers (text, buffers, enums and errors from
`caribou_abi`), and its build writes:

- the `window` model, from `xwindow_bindgen::generate(Runtime::Caribou)`;
- xwindow's native and page backends, from `xwindow_backend::install`; and
- for a page, the wire and the agent's modules, `xwindow.mjs` and
  `xwindow_wire.mjs`, which a program ships beside it.

The xwindow and x-idl revisions are pinned in the workspace `Cargo.toml`.
xwindow's README is the API reference.

## Example

```haxe
import window.*;

class Main {
    static function main() {
        var attributes = new WindowAttributes();
        attributes.title("Hello");
        attributes.width(800);
        attributes.height(600);
        var window = Window.open(attributes);

        var running = true;
        while (running) {
            switch (window.wait(0.5)) {
                case Closed:
                    running = false;
                case Resized(width, height):
                    Sys.println('${width}x${height}');
                case _:
            }
        }
        window.close();
    }
}
```

`poll()` returns the next event without waiting, and `wait(seconds)` waits
for one. Both return `None` when there is none. Sizes and positions a window
reports are in physical pixels; those a program asks for are logical.

## On a phone

By default a program pumps the event loop inside `open`, `poll` and
`wait`. A host that must build the loop, or run it, hands it over before
the first window opens, through xwindow's hook, which this crate
re-exports with the `winit` it is built from:

```rust
caribou_window::attach(events, caribou_window::Drive::Pump)?;
caribou_window::attach(events, caribou_window::Drive::Turns(&mut turn))?;
```

- **Android:** the host builds the loop with the `AndroidApp` its
  `android_main` receives. Either drive works. The plugin builds winit
  for a NativeActivity by default; a host on a GameActivity turns default
  features off and enables `android-game-activity`.
- **iOS:** winit cannot pump, so the host calls `attach` with
  `Drive::Turns` from `main`, and it never returns. A window opens only in
  a turn.

A host in C reaches the same hook through `xwindow_attach_pump` and
`xwindow_run_turns`. On Android, the `android-main` feature defines
`android_main`, which keeps the app for the backend and calls the host's
`xwindow_main`. xwindow's CONTRIBUTING.md, under "The host's hook",
describes the contract.

## In a page

Built for `wasm32-wasip1-threads`, the window is the page's canvas. Opening
it asks the host for the `xwindow` agent, and the page imports `xwindow.mjs`
beside the program and starts it. `platform()` is 7, the web canvas, which
xgpu's `GpuInstance.surface` takes as it takes a desktop window's codes.

## From the previous plugin

- `WindowBuilder` is now `WindowAttributes`, set field by field, and
  `Window.open(attributes)`.
- Methods are camelCase: `setImeAllowed`, `requestRedraw`.
- The `on_scale_factor_changed` callback is now
  `setScaleSizing(Logical | Physical)`: whether a change of scale keeps the
  window's logical size or its size in pixels.
- In a page, `platform()` is 7. The previous plugin reported 5, which xgpu
  reads as Android.

## Checks

```sh
cargo run -p caribou-plugin-fixtures --bin window
cargo run -p caribou-plugin-fixtures --bin window_gpu -- 3
cargo test -p caribou-interop --test plugin_window_events --test plugin_window_events_wren
```

The interop tests make events without opening a window. Their plugin is
xwindow's model and native backend, with a class of probes appended to the
declaration, and they check Haxe and Wren read every payload across a
collection.
