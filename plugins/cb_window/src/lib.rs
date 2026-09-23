use std::{cell::RefCell, collections::VecDeque, str::FromStr, time::Duration};

use caribou_abi::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent as NativeWindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::pump_events::EventLoopExtPumpEvents,
    window::{self, Cursor, Window, WindowAttributes},
};

pub mod events;
pub use events::*;

/// The platform codes `platform` returns.
const APPKIT: i32 = 1;
const WIN32: i32 = 2;
const XLIB: i32 = 3;
const WAYLAND: i32 = 4;

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.WindowLevel", from = window::WindowLevel)]
enum WindowLevel {
    AlwaysOnBottom,
    Normal,
    AlwaysOnTop,
}

/// Opened once, in `resumed`, because winit will not make a window before it.
struct App {
    attributes: WindowAttributes,
    window: Option<Window>,
    events: VecDeque<Event>,
    scale_callback: Option<Kept>,
    pending_error: Option<CallbackError>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = event_loop.create_window(self.attributes.clone()).ok();
        }
        self.events.push_back(Event::Resumed);
    }

    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::window::WindowId,
        mut event: NativeWindowEvent,
    ) {
        if let NativeWindowEvent::ScaleFactorChanged {
            scale_factor,
            inner_size_writer,
        } = &mut event
            && let Some(callback) = &self.scale_callback
            && self.pending_error.is_none()
        {
            let result = scale_request(callback, *scale_factor).and_then(|size| {
                if let ScaleSize::Physical { width, height } = size {
                    inner_size_writer
                        .request_inner_size(winit::dpi::PhysicalSize::new(
                            width as u32,
                            height as u32,
                        ))
                        .map_err(|_| CallbackError::Invalid("scale-change size writer expired"))?;
                }
                Ok(())
            });
            self.pending_error = result.err();
        }
        self.events.push_back(Event::from(event));
    }

    fn device_event(
        &mut self,
        _: &ActiveEventLoop,
        id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        self.events.push_back(Event::Device {
            device_id: events::device_key(id),
            event: event.into(),
        });
    }

    fn suspended(&mut self, _: &ActiveEventLoop) {
        self.events.push_back(Event::Suspended);
    }

    fn memory_warning(&mut self, _: &ActiveEventLoop) {
        self.events.push_back(Event::MemoryWarning);
    }
}

struct Open {
    event_loop: EventLoop<()>,
    app: App,
}

thread_local! {
    /// A handle is an index into this, plus one, so zero is never a window.
    /// Deliberately simpler than hlwgpu's table: a program has one window or
    /// two, not a million, and they are closed when it exits.
    static WINDOWS: RefCell<Vec<Option<Open>>> = const { RefCell::new(Vec::new()) };
}

/// Runs `body` on an open window, or returns `miss`.
fn with<T>(handle: i32, miss: T, body: impl FnOnce(&mut Open) -> T) -> T {
    WINDOWS.with(|windows| {
        let Ok(mut windows) = windows.try_borrow_mut() else {
            host::raise(
                ErrorKind::Runtime,
                "window operations cannot re-enter an active event pump",
            );
            return miss;
        };
        let index = (handle - 1).max(-1);
        if index < 0 {
            return miss;
        }
        match windows
            .get_mut(index as usize)
            .and_then(|slot| slot.as_mut())
        {
            Some(open) => body(open),
            None => miss,
        }
    })
}

#[allow(dead_code)]
fn open(title: Text, width: i32, height: i32) -> i32 {
    let Ok(event_loop) = EventLoop::new() else {
        return 0;
    };
    let attributes = Window::default_attributes()
        .with_title(title.as_str())
        .with_inner_size(winit::dpi::LogicalSize::new(width.max(1), height.max(1)));

    let mut open = Open {
        event_loop,
        app: App {
            attributes,
            window: None,
            events: VecDeque::new(),
            scale_callback: None,
            pending_error: None,
        },
    };

    // winit creates windows in `resumed`, so the loop has to run before there
    // is one. A few passes, because on some platforms it is not the first.
    for _ in 0..16 {
        open.event_loop
            .pump_app_events(Some(Duration::ZERO), &mut open.app);
        if open.app.window.is_some() {
            break;
        }
    }
    if open.app.window.is_none() {
        return 0;
    }

    WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        windows.push(Some(open));
        windows.len() as i32
    })
}

fn open_with_attributes(attributes: WindowAttributes) -> i32 {
    let Ok(event_loop) = EventLoop::new() else {
        return 0;
    };

    let mut open = Open {
        event_loop,
        app: App {
            attributes,
            window: None,
            events: VecDeque::new(),
            scale_callback: None,
            pending_error: None,
        },
    };

    // winit creates windows in `resumed`, so the loop has to run before there
    // is one. A few passes, because on some platforms it is not the first.
    for _ in 0..16 {
        open.event_loop
            .pump_app_events(Some(Duration::ZERO), &mut open.app);
        if open.app.window.is_some() {
            break;
        }
    }
    if open.app.window.is_none() {
        return 0;
    }

    WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        windows.push(Some(open));
        windows.len() as i32
    })
}

fn poll(handle: i32) -> Enum<Event> {
    let (event, error) = with(handle, (Event::None, None), |open| {
        // Drain queued events before pumping again so none are discarded.
        if open.app.events.is_empty() {
            open.event_loop
                .pump_app_events(Some(Duration::ZERO), &mut open.app);
        }
        if let Some(error) = open.app.pending_error.take() {
            (Event::None, Some(error))
        } else {
            (open.app.events.pop_front().unwrap_or(Event::None), None)
        }
    });
    if let Some(error) = error {
        error.raise();
    }
    event.into()
}

fn width(handle: i32) -> i32 {
    with(handle, 0, |open| {
        open.app
            .window
            .as_ref()
            .map_or(0, |w| w.inner_size().width as i32)
    })
}

fn height(handle: i32) -> i32 {
    with(handle, 0, |open| {
        open.app
            .window
            .as_ref()
            .map_or(0, |w| w.inner_size().height as i32)
    })
}

fn platform(handle: i32) -> i32 {
    with(handle, 0, |open| {
        let Some(window) = open.app.window.as_ref() else {
            return 0;
        };
        let Ok(raw) = window.window_handle() else {
            return 0;
        };
        match raw.as_raw() {
            RawWindowHandle::AppKit(_) => APPKIT,
            RawWindowHandle::Win32(_) => WIN32,
            RawWindowHandle::Xlib(_) => XLIB,
            RawWindowHandle::Wayland(_) => WAYLAND,
            _ => 0,
        }
    })
}

/// 0 and 1 are the window handle's fields, 2 and 3 the display's.
///
/// Reported as plain integers because the two libraries are separate: a Rust
/// type cannot cross between them, but the pointer inside it can.
extern "C" fn raw(handle: i32, which: i32) -> i64 {
    with(handle, 0, |open| {
        let Some(window) = open.app.window.as_ref() else {
            return 0;
        };
        match which {
            0 | 1 => match window.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::AppKit(h)) if which == 0 => h.ns_view.as_ptr() as i64,
                Ok(RawWindowHandle::Win32(h)) if which == 0 => h.hwnd.get() as i64,
                Ok(RawWindowHandle::Win32(h)) => h.hinstance.map_or(0, |v| v.get() as i64),
                Ok(RawWindowHandle::Xlib(h)) if which == 0 => h.window as i64,
                Ok(RawWindowHandle::Xlib(h)) => h.visual_id as i64,
                Ok(RawWindowHandle::Wayland(h)) if which == 0 => h.surface.as_ptr() as i64,
                _ => 0,
            },
            2 | 3 => match window.display_handle().map(|h| h.as_raw()) {
                Ok(RawDisplayHandle::Xlib(h)) if which == 2 => {
                    h.display.map_or(0, |p| p.as_ptr() as i64)
                }
                Ok(RawDisplayHandle::Xlib(h)) => h.screen as i64,
                Ok(RawDisplayHandle::Wayland(h)) if which == 2 => h.display.as_ptr() as i64,
                _ => 0,
            },
            _ => 0,
        }
    })
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
        with(this.handle, Text::new(""), |open| {
            open.app
                .window
                .as_ref()
                .and_then(|w| w.current_monitor())
                .and_then(|m| m.name())
                .map_or(Text::new(""), |s| Text::new(&s))
        })
    }

    pub extern "C" fn size(this: &MonitorHandle) -> Box<Size> {
        with(
            this.handle,
            Box::new(Size {
                width: 0,
                height: 0,
            }),
            |open| {
                open.app
                    .window
                    .as_ref()
                    .and_then(|w| w.current_monitor())
                    .map_or(
                        Box::new(Size {
                            width: 0,
                            height: 0,
                        }),
                        |m| {
                            let size = m.size();
                            Box::new(Size {
                                width: size.width as i32,
                                height: size.height as i32,
                            })
                        },
                    )
            },
        )
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
        poll(this.handle)
    }

    pub extern "C" fn width(this: &WindowHandle) -> i32 {
        width(this.handle)
    }

    pub extern "C" fn height(this: &WindowHandle) -> i32 {
        height(this.handle)
    }

    pub extern "C" fn platform(this: &WindowHandle) -> i32 {
        platform(this.handle)
    }

    pub extern "C" fn raw(this: &WindowHandle, which: i32) -> i64 {
        raw(this.handle, which)
    }
    pub extern "C" fn set_cursor_icon(this: &WindowHandle, icon: Text) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                window.set_cursor(
                    winit::window::CursorIcon::from_str(icon.as_str())
                        .unwrap_or(winit::window::CursorIcon::Default),
                );
            }
        });
    }

    pub extern "C" fn set_cursor_custom(
        this: &WindowHandle,
        rgba: caribou_abi::Buffer,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
    ) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                if let Ok(cursor) = winit::window::CustomCursor::from_rgba(
                    rgba.to_vec(),
                    width,
                    height,
                    hotspot_x,
                    hotspot_y,
                ) {
                    window.set_cursor(Cursor::Custom(open.event_loop.create_custom_cursor(cursor)));
                }
            }
        });
    }

    pub extern "C" fn set_position(this: &WindowHandle, x: i32, y: i32) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                window.set_outer_position(winit::dpi::LogicalPosition::new(x, y));
            }
        });
    }

    pub extern "C" fn set_size(this: &WindowHandle, width: i32, height: i32) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                let _ = window
                    .request_inner_size(winit::dpi::LogicalSize::new(width.max(0), height.max(0)));
            }
        });
    }

    pub extern "C" fn request_redraw(this: &WindowHandle) {
        with(this.handle, (), |open| {
            if let Some(window) = &open.app.window {
                window.request_redraw();
            }
        });
    }

    pub extern "C" fn set_ime_allowed(this: &WindowHandle, allowed: bool) {
        with(this.handle, (), |open| {
            if let Some(window) = &open.app.window {
                window.set_ime_allowed(allowed);
            }
        });
    }

    pub extern "C" fn set_ime_cursor_area(
        this: &WindowHandle,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) {
        with(this.handle, (), |open| {
            if let Some(window) = &open.app.window {
                window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(x, y),
                    winit::dpi::LogicalSize::new(width.max(0.0), height.max(0.0)),
                );
            }
        });
    }

    /// The callback runs synchronously during poll and returns ScaleSize.
    /// A null value removes it. Window operations cannot re-enter this callback.
    pub extern "C" fn on_scale_factor_changed(this: &WindowHandle, callback: Value) {
        with(this.handle, (), |open| {
            open.app.scale_callback = (!callback.is_null()).then(|| Kept::new(callback));
        });
    }

    /// Returns a correlation ID, or zero when startup notification is unsupported.
    pub extern "C" fn request_activation_token(this: &WindowHandle) -> i64 {
        with(this.handle, 0, |open| {
            #[cfg(any(
                target_os = "linux",
                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            {
                use winit::platform::startup_notify::WindowExtStartupNotify;
                open.app
                    .window
                    .as_ref()
                    .and_then(|w| w.request_activation_token().ok())
                    .map_or(0, events::request_key)
            }
            #[cfg(not(any(
                target_os = "linux",
                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "netbsd",
                target_os = "openbsd"
            )))]
            {
                let _ = open;
                0
            }
        })
    }

    pub extern "C" fn set_fullscreen(this: &WindowHandle, yes: bool) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                let fullscreen = if yes {
                    Some(winit::window::Fullscreen::Borderless(None))
                } else {
                    None
                };
                window.set_fullscreen(fullscreen);
            }
        });
    }

    pub extern "C" fn has_focus(this: &WindowHandle) -> bool {
        with(this.handle, false, |open| {
            open.app.window.as_ref().map_or(false, |w| w.has_focus())
        })
    }

    pub extern "C" fn focus(this: &WindowHandle) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                window.focus_window();
            }
        });
    }

    pub extern "C" fn scale_factor(this: &WindowHandle) -> f64 {
        with(this.handle, 1.0, |open| {
            open.app.window.as_ref().map_or(1.0, |w| w.scale_factor())
        })
    }

    pub extern "C" fn set_blur(this: &WindowHandle, yes: bool) {
        with(this.handle, (), |open| {
            if let Some(window) = open.app.window.as_ref() {
                window.set_blur(yes);
            }
        });
    }

    pub extern "C" fn current_monitor(this: &WindowHandle) -> Box<MonitorHandle> {
        let handle = with(this.handle, 0, |open| {
            open.app
                .window
                .as_ref()
                .and_then(|w| w.current_monitor())
                .map_or(0, |_| this.handle)
        });
        Box::new(MonitorHandle { handle })
    }

    pub extern "C" fn close(this: &WindowHandle) {
        with(this.handle, (), |open| {
            open.app.window = None;
            open.app.scale_callback = None;
            open.app.pending_error = None;
        });
    }
}

#[derive(Clone)]
struct WindowBuilder {
    attributes: WindowAttributes,
}

impl WindowBuilder {
    pub extern "C" fn new() -> Box<WindowBuilder> {
        Box::new(WindowBuilder {
            attributes: WindowAttributes::default(),
        })
    }

    pub extern "C" fn title(this: &mut WindowBuilder, title: Text) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_title(title.as_str());
        let new_box = Box::new(WindowBuilder {
            attributes: this.attributes.clone(),
        });
        let _ = this;
        new_box
    }

    pub extern "C" fn size(
        this: &mut WindowBuilder,
        width: i32,
        height: i32,
    ) -> Box<WindowBuilder> {
        this.attributes = this
            .attributes
            .clone()
            .with_inner_size(winit::dpi::LogicalSize::new(width, height));
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn fullscreen(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        let fullscreen = if yes {
            Some(winit::window::Fullscreen::Borderless(None))
        } else {
            None
        };
        this.attributes = this.attributes.clone().with_fullscreen(fullscreen);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn resizable(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_resizable(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn maximized(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_maximized(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn visible(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_visible(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn content_protected(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_content_protected(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn decorations(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_decorations(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn transparent(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_transparent(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn blur(this: &mut WindowBuilder, yes: bool) -> Box<WindowBuilder> {
        this.attributes = this.attributes.clone().with_blur(yes);
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn min_size(
        this: &mut WindowBuilder,
        width: i32,
        height: i32,
    ) -> Box<WindowBuilder> {
        this.attributes = this
            .attributes
            .clone()
            .with_min_inner_size(winit::dpi::LogicalSize::new(width, height));
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn max_size(
        this: &mut WindowBuilder,
        width: i32,
        height: i32,
    ) -> Box<WindowBuilder> {
        this.attributes = this
            .attributes
            .clone()
            .with_max_inner_size(winit::dpi::LogicalSize::new(width, height));
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn window_level(
        this: &mut WindowBuilder,
        level: Enum<WindowLevel>,
    ) -> Box<WindowBuilder> {
        let window_level: window::WindowLevel = match level.get() {
            WindowLevel::AlwaysOnBottom => window::WindowLevel::AlwaysOnBottom,
            WindowLevel::Normal => window::WindowLevel::Normal,
            WindowLevel::AlwaysOnTop => window::WindowLevel::AlwaysOnTop,
        };
        this.attributes = this
            .attributes
            .clone()
            .with_window_level(window_level.into());
        let new_box = Box::new(this.clone());
        let _ = this;
        new_box
    }

    pub extern "C" fn open(this: &mut WindowBuilder) -> Box<WindowHandle> {
        let handle = open_with_attributes(this.attributes.clone());

        Box::new(WindowHandle { handle })
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
