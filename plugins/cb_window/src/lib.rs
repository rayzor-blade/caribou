//! The window plugin: a window, its events, and what can be asked of it.
//! On a desktop it is winit's (`native`); in a page, the page's canvas, as
//! Ash's page reports it (`web`).

use caribou_abi::*;

pub mod events;
pub use events::*;

#[cfg(not(target_os = "wasi"))]
mod native;
#[cfg(not(target_os = "wasi"))]
use native as backend;
#[cfg(target_os = "wasi")]
mod web;
#[cfg(target_os = "wasi")]
use web as backend;

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.WindowLevel")]
#[cfg_attr(not(target_os = "wasi"), caribou(from = winit::window::WindowLevel))]
enum WindowLevel {
    AlwaysOnBottom,
    Normal,
    AlwaysOnTop,
}

struct Size {
    width: i32,
    height: i32,
}

impl Size {
    pub extern "C" fn width(this: &Size) -> i32 {
        this.width
    }

    pub extern "C" fn height(this: &Size) -> i32 {
        this.height
    }
}

struct MonitorHandle {
    handle: i32,
}

impl MonitorHandle {
    pub extern "C" fn name(this: &MonitorHandle) -> Text {
        Text::new(&backend::monitor_name(this.handle))
    }

    pub extern "C" fn size(this: &MonitorHandle) -> Box<Size> {
        let (width, height) = backend::monitor_size(this.handle);
        Box::new(Size { width, height })
    }
}

struct WindowHandle {
    handle: i32,
}

impl WindowHandle {
    // Factories also let frontends without enum constructor syntax supply
    // the synchronous callback's result.
    pub extern "C" fn physical_scale_size(width: i32, height: i32) -> Enum<ScaleSize> {
        ScaleSize::Physical { width, height }.into()
    }

    pub extern "C" fn default_scale_size() -> Enum<ScaleSize> {
        ScaleSize::Default.into()
    }

    pub extern "C" fn poll(this: &WindowHandle) -> Enum<Event> {
        backend::poll(this.handle)
    }

    pub extern "C" fn width(this: &WindowHandle) -> i32 {
        backend::width(this.handle)
    }

    pub extern "C" fn height(this: &WindowHandle) -> i32 {
        backend::height(this.handle)
    }

    pub extern "C" fn platform(this: &WindowHandle) -> i32 {
        backend::platform(this.handle)
    }

    /// 0 and 1 are the window handle's fields, 2 and 3 the display's.
    ///
    /// Reported as plain integers because the two libraries are separate: a
    /// Rust type cannot cross between them, but the pointer inside it can.
    pub extern "C" fn raw(this: &WindowHandle, which: i32) -> i64 {
        backend::raw(this.handle, which)
    }

    pub extern "C" fn set_cursor_icon(this: &WindowHandle, icon: Text) {
        backend::set_cursor_icon(this.handle, icon.as_str());
    }

    pub extern "C" fn set_cursor_custom(
        this: &WindowHandle,
        rgba: caribou_abi::Buffer,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
    ) {
        backend::set_cursor_custom(
            this.handle,
            rgba.to_vec(),
            width,
            height,
            hotspot_x,
            hotspot_y,
        );
    }

    pub extern "C" fn set_position(this: &WindowHandle, x: i32, y: i32) {
        backend::set_position(this.handle, x, y);
    }

    pub extern "C" fn set_size(this: &WindowHandle, width: i32, height: i32) {
        backend::set_size(this.handle, width, height);
    }

    pub extern "C" fn request_redraw(this: &WindowHandle) {
        backend::request_redraw(this.handle);
    }

    pub extern "C" fn set_ime_allowed(this: &WindowHandle, allowed: bool) {
        backend::set_ime_allowed(this.handle, allowed);
    }

    pub extern "C" fn set_ime_cursor_area(
        this: &WindowHandle,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) {
        backend::set_ime_cursor_area(this.handle, x, y, width, height);
    }

    /// The callback runs synchronously during poll and returns ScaleSize.
    /// A null value removes it. Window operations cannot re-enter this callback.
    pub extern "C" fn on_scale_factor_changed(this: &WindowHandle, callback: Value) {
        backend::on_scale_factor_changed(
            this.handle,
            (!callback.is_null()).then(|| Kept::new(callback)),
        );
    }

    /// Returns a correlation ID, or zero when startup notification is unsupported.
    pub extern "C" fn request_activation_token(this: &WindowHandle) -> i64 {
        backend::request_activation_token(this.handle)
    }

    pub extern "C" fn set_fullscreen(this: &WindowHandle, yes: bool) {
        backend::set_fullscreen(this.handle, yes);
    }

    pub extern "C" fn has_focus(this: &WindowHandle) -> bool {
        backend::has_focus(this.handle)
    }

    pub extern "C" fn focus(this: &WindowHandle) {
        backend::focus(this.handle);
    }

    pub extern "C" fn scale_factor(this: &WindowHandle) -> f64 {
        backend::scale_factor(this.handle)
    }

    pub extern "C" fn set_blur(this: &WindowHandle, yes: bool) {
        backend::set_blur(this.handle, yes);
    }

    pub extern "C" fn current_monitor(this: &WindowHandle) -> Box<MonitorHandle> {
        Box::new(MonitorHandle {
            handle: backend::current_monitor(this.handle),
        })
    }

    pub extern "C" fn close(this: &WindowHandle) {
        backend::close(this.handle);
    }
}

#[derive(Clone)]
struct WindowBuilder {
    attributes: backend::Attributes,
}

impl WindowBuilder {
    /// The builder with `change` applied, as the next link in the chain.
    fn with(
        this: &mut WindowBuilder,
        change: impl FnOnce(backend::Attributes) -> backend::Attributes,
    ) -> Box<WindowBuilder> {
        this.attributes = change(this.attributes.clone());
        Box::new(this.clone())
    }

    pub extern "C" fn new() -> Box<WindowBuilder> {
        Box::new(WindowBuilder {
            attributes: backend::Attributes::default(),
        })
    }

    pub extern "C" fn title(this: &mut WindowBuilder, title: Text) -> Box<WindowBuilder> {
        Self::with(this, |a| a.title(title.as_str()))
    }

    pub extern "C" fn size(
        this: &mut WindowBuilder,
        width: i32,
        height: i32,
    ) -> Box<WindowBuilder> {
        Self::with(this, |a| a.size(width, height))
    }

    pub extern "C" fn fullscreen(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.fullscreen(yes))
    }

    pub extern "C" fn resizable(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.resizable(yes))
    }

    pub extern "C" fn maximized(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.maximized(yes))
    }

    pub extern "C" fn visible(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.visible(yes))
    }

    pub extern "C" fn content_protected(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.content_protected(yes))
    }

    pub extern "C" fn decorations(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.decorations(yes))
    }

    pub extern "C" fn transparent(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.transparent(yes))
    }

    pub extern "C" fn blur(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        Self::with(this, |a| a.blur(yes))
    }

    pub extern "C" fn min_size(
        this: &mut WindowBuilder,
        width: i32,
        height: i32,
    ) -> Box<WindowBuilder> {
        Self::with(this, |a| a.min_size(width, height))
    }

    pub extern "C" fn max_size(
        this: &mut WindowBuilder,
        width: i32,
        height: i32,
    ) -> Box<WindowBuilder> {
        Self::with(this, |a| a.max_size(width, height))
    }

    pub extern "C" fn window_level(
        this: &mut WindowBuilder,
        level: Enum<WindowLevel>,
    ) -> Box<WindowBuilder> {
        Self::with(this, |a| a.window_level(level.get()))
    }

    pub extern "C" fn open(this: &mut WindowBuilder) -> Box<WindowHandle> {
        Box::new(WindowHandle {
            handle: backend::open(this.attributes.clone()),
        })
    }
}

caribou_abi::plugin! {
    name:"window";


    enum Event;
    enum MouseButton;
    enum MouseElementState;
    enum MouseScrollDelta;
    enum TouchPhase;
    enum FilePath;
    enum OptionalText;
    enum OptionalFloat;
    enum CursorRange;
    enum Ime;
    enum TouchForce;
    enum Theme;
    enum NativeKeyCode;
    enum NativeKey;
    enum PhysicalKey;
    enum Key;
    enum KeyCode;
    enum NamedKey;
    enum KeyLocation;
    enum KeySupplement;
    enum KeyEvent;
    enum ModifiersKeyState;
    enum Modifiers;
    enum DeviceEvent;
    enum ScaleSize;
    enum WindowLevel;

    class Size {
        fn width(&Size) -> i32;
        fn height(&Size) -> i32;
    }

    class MonitorHandle {
        fn name(&MonitorHandle) -> Text;
        fn size(&MonitorHandle) -> Box<Size>;
    }

    class WindowHandle {
        fn physical_scale_size(i32, i32) -> Enum<ScaleSize>;
        fn default_scale_size() -> Enum<ScaleSize>;
        fn poll(&WindowHandle) -> Enum<Event>;
        fn width(&WindowHandle) -> i32;
        fn height(&WindowHandle) -> i32;
        fn platform(&WindowHandle) -> i32;
        fn raw(&WindowHandle, i32) -> i64;
        fn set_cursor_icon(&WindowHandle, Text);
        fn set_cursor_custom(&WindowHandle, Buffer, u16, u16, u16, u16);
        fn set_position(&WindowHandle, i32, i32);
        fn set_size(&WindowHandle, i32, i32);
        fn request_redraw(&WindowHandle);
        fn set_ime_allowed(&WindowHandle, bool);
        fn set_ime_cursor_area(&WindowHandle, f64, f64, f64, f64);
        fn on_scale_factor_changed(&WindowHandle, Value);
        fn request_activation_token(&WindowHandle) -> i64;
        fn set_fullscreen(&WindowHandle, bool);
        fn has_focus(&WindowHandle) -> bool;
        fn focus(&WindowHandle);
        fn scale_factor(&WindowHandle) -> f64;
        fn set_blur(&WindowHandle, bool);
        fn current_monitor(&WindowHandle) -> Box<MonitorHandle>;
        fn close(&WindowHandle);
    }

    class WindowBuilder {
        fn new() -> Box<WindowBuilder>;
        fn title(&mut WindowBuilder, Text) -> Box<WindowBuilder>;
        fn size(&mut WindowBuilder, i32, i32) -> Box<WindowBuilder>;
        fn fullscreen(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn resizable(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn maximized(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn visible(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn content_protected(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn decorations(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn transparent(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn blur(&mut WindowBuilder, bool) -> Box<WindowBuilder>;
        fn min_size(&mut WindowBuilder, i32, i32) -> Box<WindowBuilder>;
        fn max_size(&mut WindowBuilder, i32, i32) -> Box<WindowBuilder>;
        fn window_level(&mut WindowBuilder, Enum<WindowLevel>) -> Box<WindowBuilder>;
        fn open(&mut WindowBuilder) -> Box<WindowHandle>;
    }
}
