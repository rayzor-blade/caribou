# Browser API input for the Caribou window plugin

This directory holds the browser APIs the window plugin maps to when a
program runs in a page. `window.idl` contains whole definitions, verbatim,
from each specification's IDL, as W3C's
[webref](https://github.com/w3c/webref) collects it: the curated branch at
revision `89e7d68c6612fbedb740c1632d36e482ca39e999` (2026-09-22). Each
section of the file names its specification and licence.

The plugin's native backend is winit. Its web backend (`src/web.rs`) reaches
these APIs through Ash's page, which owns the canvas and its events and
writes them into a block of the program's memory in the format of Ash's
`docs/wasm/window.md`. This file is the reference for that mapping.
`xgpu_bindgen::idl` parses it whole, and `xgpu_bindgen::wire`
generates a wire from it.

## Where things run in a page

A program runs in a Worker (see Ash's page). Three parts of the page touch
the window, on three different threads:

- **The page's own thread** holds the `<canvas>` element. DOM events fire
  only there: pointer, wheel, keyboard, focus, drag and drop, resize,
  visibility, and the media queries for scale and theme. The window's agent
  runs here. It listens on the canvas element and the document, and writes
  each event into a queue in the program's shared memory, which `poll()`
  reads without a call into JavaScript.
- **The GPU agent**, a Worker, holds the canvas's `OffscreenCanvas` after
  the page transfers it (`transferControlToOffscreen`). Once transferred,
  only the OffscreenCanvas's owner can set its `width` and `height`, the
  size of the drawing buffer. So a resize the page sees has to reach the
  owner as well. The element's CSS size stays the page's.
- **The program** reads events, and asks for changes: title, cursor,
  fullscreen, size. The window's agent carries out each change on the page's
  thread.

Things that need a user's gesture, which are fullscreen, pointer lock and
focus, only work while the page is handling that gesture. A request arriving
later over the wire is refused by the browser. The agent performs such a
request inside the next input event's handler.

## The plugin's members

| Plugin member | Browser API | Notes |
| --- | --- | --- |
| `WindowBuilder.open` | The page's `<canvas>` element | A page has one window: its canvas. |
| `title` | `Document.title` | |
| `size`, `WindowHandle.set_size` | `HTMLElement.style` (`CSSStyleDeclaration`) width and height, and `OffscreenCanvas.width`/`height` at its owner | Logical size in CSS pixels; the drawing buffer is that size times `devicePixelRatio`. |
| `min_size`, `max_size` | `style` min/max width and height | |
| `visible` | `HTMLElement.hidden` | |
| `fullscreen`, `set_fullscreen` | `Element.requestFullscreen`, `Document.exitFullscreen`, `Document.fullscreenElement` | Needs a user gesture. |
| `transparent` | The GPU context's `alphaMode` (`premultiplied`) | Configured on the GPU side. |
| `resizable`, `maximized`, `decorations`, `blur`, `set_blur`, `content_protected`, `window_level`, `set_position` | None | A page cannot place or decorate its window. |
| `WindowHandle.width`, `height` | `ResizeObserverEntry.devicePixelContentBoxSize` | Physical pixels, as winit reports them. |
| `scale_factor` | `Window.devicePixelRatio` | |
| `platform`, `raw` | The canvas | raw-window-handle's web handles name a canvas; the GPU plugin's web surface is the page's canvas anyway. |
| `request_redraw` | `AnimationFrameProvider.requestAnimationFrame` | Queues `RedrawRequested` at the next frame. |
| `set_cursor_icon` | `style.cursor` | CSS cursor names cover winit's `CursorIcon`. |
| `set_cursor_custom` | `style.cursor = url(...) x y` | The RGBA pixels become an image (`OffscreenCanvas.convertToBlob`, `URL.createObjectURL`). |
| `set_ime_allowed`, `set_ime_cursor_area` | `HTMLElement.editContext` (`EditContext`), `updateControlBounds`, `updateSelectionBounds` | The composition window follows the given rectangle. |
| `has_focus`, `focus` | `Document.hasFocus`, `HTMLOrSVGOrMathMLElement.focus` | The canvas needs a `tabIndex` to take focus. |
| `current_monitor`, `MonitorHandle.size` | `Window.screen` (`Screen.width`, `height`) | |
| `MonitorHandle.name` | `Window.getScreenDetails`, `ScreenDetailed.label` | Needs the window-management permission; otherwise unknown. |
| `on_scale_factor_changed` | `Window.matchMedia("(resolution: <n>dppx)")` change | The page cannot size itself on a scale change the way winit's callback does, so the callback's answer only sets the canvas size. |
| `request_activation_token` | None | Returns 0, as on platforms without one. |
| `close` | None | A page ends when it is closed or navigated away. |

## The plugin's events

| Event | Browser event | Notes |
| --- | --- | --- |
| `Resized` | `ResizeObserver` on the canvas, `device-pixel-content-box` | Physical size. |
| `Moved` | None | |
| `Closed`, `Destroyed` | `pagehide` (`PageTransitionEvent`) | |
| `Focused` | `focus`, `blur` (`FocusEvent`) on the canvas | |
| `Occluded` | `visibilitychange`, `Document.visibilityState` | |
| `CursorEntered`, `CursorLeft` | `pointerenter`, `pointerleave` (`PointerEvent`) | The device id is `pointerId`. |
| `CursorMoved` | `pointermove`, `PointerEvent.getCoalescedEvents` | Position is `offsetX`/`offsetY` times `devicePixelRatio`. |
| `MouseInput` | `pointerdown`, `pointerup` for mouse pointers | `button` 0, 1, 2, 3 and 4 are Left, Middle, Right, Back and Forward. |
| `MouseWheel` | `wheel` (`WheelEvent`) | `deltaMode` line gives `LineDelta`, pixel gives `PixelDelta`. |
| `KeyboardInput` | `keydown`, `keyup` (`KeyboardEvent`) | `code` gives the physical key, `key` the logical key, then `location` and `repeat`; `isTrusted` false means synthetic. |
| `ModifiersChanged` | `KeyboardEvent.getModifierState`, `shiftKey`, `ctrlKey`, `altKey`, `metaKey` | Sent when the flags change. Left and right come from `location`. |
| `Ime` | `EditContext` `textupdate`, `compositionstart`, `compositionend` (`TextUpdateEvent`, `CompositionEvent`) | Preedit carries the selection. |
| `DroppedFile`, `HoveredFile`, `HoveredFileCancelled` | `drop`, `dragover`, `dragleave` (`DragEvent`, `DataTransfer.files`) | A page gets a file's name and contents, never its path. |
| `Touch` | `pointerdown`, `pointermove`, `pointerup`, `pointercancel` for touch pointers | `pressure` gives `TouchForce.Normalized`; `TouchEvent` and `Touch` are the older equivalent. |
| `TouchpadPressure` | `PointerEvent.pressure` | |
| `PinchGesture` | `wheel` with `ctrlKey` | Browsers report a trackpad pinch that way. |
| `PanGesture`, `RotationGesture`, `DoubleTapGesture` | None standard | |
| `ScaleFactorChanged` | `matchMedia("(resolution: <n>dppx)")` change (`MediaQueryListEvent`) | |
| `ThemeChanged` | `matchMedia("(prefers-color-scheme: dark)")` change | |
| `RedrawRequested` | The `requestAnimationFrame` callback | |
| `Device(MouseMotion)` | `PointerEvent.movementX`/`movementY` under `Element.requestPointerLock` | Needs a user gesture. |
| `Resumed`, `Suspended` | `pageshow`, `pagehide` | |
| `AxisMotion`, `ActivationTokenDone`, `MemoryWarning`, other `Device` events | None | |

## Updating the snapshot

Take the same definitions from a newer webref revision, and record the
revision here and in the file's header. Definitions move between
specifications. For example, `MouseEvent` and `WheelEvent` are now in Pointer
Events, not UI Events. Check each one, then run xgpu-bindgen's tests,
which parse this file and generate its wire. The build never rewrites the
file.
