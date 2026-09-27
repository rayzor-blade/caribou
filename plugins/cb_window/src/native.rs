//! The window on a desktop: winit, pumped from `poll`.

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

use crate::WindowLevel;
use crate::events::{self, CallbackError, Event, ScaleSize, scale_request};

/// The platform codes `platform` returns; a page's canvas is 5 (`web`).
const APPKIT: i32 = 1;
const WIN32: i32 = 2;
const XLIB: i32 = 3;
const WAYLAND: i32 = 4;

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

pub(crate) fn poll(handle: i32) -> Enum<Event> {
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

pub(crate) fn width(handle: i32) -> i32 {
    with(handle, 0, |open| {
        open.app
            .window
            .as_ref()
            .map_or(0, |w| w.inner_size().width as i32)
    })
}

pub(crate) fn height(handle: i32) -> i32 {
    with(handle, 0, |open| {
        open.app
            .window
            .as_ref()
            .map_or(0, |w| w.inner_size().height as i32)
    })
}

pub(crate) fn platform(handle: i32) -> i32 {
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
pub(crate) fn raw(handle: i32, which: i32) -> i64 {
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

/// What a window opens with.
#[derive(Clone, Default)]
pub(crate) struct Attributes(WindowAttributes);

impl Attributes {
    pub(crate) fn title(self, title: &str) -> Self {
        Self(self.0.with_title(title))
    }
    pub(crate) fn size(self, width: i32, height: i32) -> Self {
        Self(
            self.0
                .with_inner_size(winit::dpi::LogicalSize::new(width, height)),
        )
    }
    pub(crate) fn fullscreen(self, yes: bool) -> Self {
        Self(
            self.0
                .with_fullscreen(yes.then_some(winit::window::Fullscreen::Borderless(None))),
        )
    }
    pub(crate) fn resizable(self, yes: bool) -> Self {
        Self(self.0.with_resizable(yes))
    }
    pub(crate) fn maximized(self, yes: bool) -> Self {
        Self(self.0.with_maximized(yes))
    }
    pub(crate) fn visible(self, yes: bool) -> Self {
        Self(self.0.with_visible(yes))
    }
    pub(crate) fn content_protected(self, yes: bool) -> Self {
        Self(self.0.with_content_protected(yes))
    }
    pub(crate) fn decorations(self, yes: bool) -> Self {
        Self(self.0.with_decorations(yes))
    }
    pub(crate) fn transparent(self, yes: bool) -> Self {
        Self(self.0.with_transparent(yes))
    }
    pub(crate) fn blur(self, yes: bool) -> Self {
        Self(self.0.with_blur(yes))
    }
    pub(crate) fn min_size(self, width: i32, height: i32) -> Self {
        Self(
            self.0
                .with_min_inner_size(winit::dpi::LogicalSize::new(width, height)),
        )
    }
    pub(crate) fn max_size(self, width: i32, height: i32) -> Self {
        Self(
            self.0
                .with_max_inner_size(winit::dpi::LogicalSize::new(width, height)),
        )
    }
    pub(crate) fn window_level(self, level: WindowLevel) -> Self {
        Self(self.0.with_window_level(match level {
            WindowLevel::AlwaysOnBottom => window::WindowLevel::AlwaysOnBottom,
            WindowLevel::Normal => window::WindowLevel::Normal,
            WindowLevel::AlwaysOnTop => window::WindowLevel::AlwaysOnTop,
        }))
    }
}

pub(crate) fn open(attributes: Attributes) -> i32 {
    open_with_attributes(attributes.0)
}

pub(crate) fn monitor_name(handle: i32) -> String {
    with(handle, String::new(), |open| {
        open.app
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .and_then(|m| m.name())
            .unwrap_or_default()
    })
}

pub(crate) fn monitor_size(handle: i32) -> (i32, i32) {
    with(handle, (0, 0), |open| {
        open.app
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .map_or((0, 0), |m| {
                let size = m.size();
                (size.width as i32, size.height as i32)
            })
    })
}

pub(crate) fn set_cursor_icon(handle: i32, icon: &str) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref() {
            window.set_cursor(
                winit::window::CursorIcon::from_str(icon)
                    .unwrap_or(winit::window::CursorIcon::Default),
            );
        }
    });
}

pub(crate) fn set_cursor_custom(
    handle: i32,
    rgba: Vec<u8>,
    width: u16,
    height: u16,
    hotspot_x: u16,
    hotspot_y: u16,
) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref()
            && let Ok(cursor) =
                winit::window::CustomCursor::from_rgba(rgba, width, height, hotspot_x, hotspot_y)
        {
            window.set_cursor(Cursor::Custom(open.event_loop.create_custom_cursor(cursor)));
        }
    });
}

pub(crate) fn set_position(handle: i32, x: i32, y: i32) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref() {
            window.set_outer_position(winit::dpi::LogicalPosition::new(x, y));
        }
    });
}

pub(crate) fn set_size(handle: i32, width: i32, height: i32) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref() {
            let _ = window
                .request_inner_size(winit::dpi::LogicalSize::new(width.max(0), height.max(0)));
        }
    });
}

pub(crate) fn request_redraw(handle: i32) {
    with(handle, (), |open| {
        if let Some(window) = &open.app.window {
            window.request_redraw();
        }
    });
}

pub(crate) fn set_ime_allowed(handle: i32, allowed: bool) {
    with(handle, (), |open| {
        if let Some(window) = &open.app.window {
            window.set_ime_allowed(allowed);
        }
    });
}

pub(crate) fn set_ime_cursor_area(handle: i32, x: f64, y: f64, width: f64, height: f64) {
    with(handle, (), |open| {
        if let Some(window) = &open.app.window {
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(x, y),
                winit::dpi::LogicalSize::new(width.max(0.0), height.max(0.0)),
            );
        }
    });
}

pub(crate) fn on_scale_factor_changed(handle: i32, callback: Option<Kept>) {
    with(handle, (), |open| open.app.scale_callback = callback);
}

/// A correlation ID, or zero when startup notification is unsupported.
pub(crate) fn request_activation_token(handle: i32) -> i64 {
    with(handle, 0, |open| {
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

pub(crate) fn set_fullscreen(handle: i32, yes: bool) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref() {
            window.set_fullscreen(yes.then_some(winit::window::Fullscreen::Borderless(None)));
        }
    });
}

pub(crate) fn has_focus(handle: i32) -> bool {
    with(handle, false, |open| {
        open.app.window.as_ref().is_some_and(|w| w.has_focus())
    })
}

pub(crate) fn focus(handle: i32) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref() {
            window.focus_window();
        }
    });
}

pub(crate) fn scale_factor(handle: i32) -> f64 {
    with(handle, 1.0, |open| {
        open.app.window.as_ref().map_or(1.0, |w| w.scale_factor())
    })
}

pub(crate) fn set_blur(handle: i32, yes: bool) {
    with(handle, (), |open| {
        if let Some(window) = open.app.window.as_ref() {
            window.set_blur(yes);
        }
    });
}

/// `handle` when the window is on a monitor, else zero.
pub(crate) fn current_monitor(handle: i32) -> i32 {
    with(handle, 0, |open| {
        open.app
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .map_or(0, |_| handle)
    })
}

pub(crate) fn close(handle: i32) {
    with(handle, (), |open| {
        open.app.window = None;
        open.app.scale_callback = None;
        open.app.pending_error = None;
    });
}
