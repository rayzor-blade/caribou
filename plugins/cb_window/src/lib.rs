use std::{cell::RefCell, collections::VecDeque, str::FromStr, time::Duration};

use caribou_abi::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent as NativeWindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::pump_events::EventLoopExtPumpEvents,
    window::{Window, WindowAttributes},
};

// These declarations generate both the Caribou schema and From<native>
// conversion. Payloads stay ordinary Rust values while queued; poll encodes
// them into the shared heap, rooting each nested enum before the next.
#[derive(PluginEnum)]
#[caribou(name = "window.MouseButton", from = winit::event::MouseButton)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(#[caribou(name = "button")] u16),
}

#[derive(PluginEnum)]
#[caribou(name = "window.MouseElementState", from = winit::event::ElementState)]
pub enum MouseElementState {
    Pressed,
    Released,
}

#[derive(PluginEnum)]
#[caribou(name = "window.MouseScrollDelta", from = winit::event::MouseScrollDelta)]
pub enum MouseScrollDelta {
    LineDelta(#[caribou(name = "x")] f32, #[caribou(name = "y")] f32),
    #[caribou(pattern = winit::event::MouseScrollDelta::PixelDelta(position))]
    PixelDelta {
        #[caribou(value = position.x)]
        x: f64,
        #[caribou(value = position.y)]
        y: f64,
    },
}

#[derive(PluginEnum)]
#[caribou(name = "window.TouchPhase", from = winit::event::TouchPhase)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

#[derive(PluginEnum)]
#[caribou(name = "window.Event", from = NativeWindowEvent, fallback = Self::None)]
pub enum Event {
    #[caribou(skip)]
    None,
    #[caribou(pattern = NativeWindowEvent::CloseRequested)]
    Closed,
    #[caribou(pattern = NativeWindowEvent::Resized(size))]
    Resized {
        #[caribou(value = size.width as i32)]
        width: i32,
        #[caribou(value = size.height as i32)]
        height: i32,
    },
    #[caribou(pattern = NativeWindowEvent::Moved(position))]
    Moved {
        #[caribou(value = position.x)]
        x: i32,
        #[caribou(value = position.y)]
        y: i32,
    },
    #[caribou(pattern = NativeWindowEvent::CursorEntered { .. })]
    CursorEntered,
    #[caribou(pattern = NativeWindowEvent::CursorLeft { .. })]
    CursorLeft,
    #[caribou(pattern = NativeWindowEvent::CursorMoved { position, .. })]
    CursorMoved {
        #[caribou(value = position.x)]
        x: f64,
        #[caribou(value = position.y)]
        y: f64,
    },
    #[caribou(pattern = NativeWindowEvent::MouseInput { state, button, .. })]
    MouseInput {
        state: MouseElementState,
        button: MouseButton,
    },
    #[caribou(pattern = NativeWindowEvent::MouseWheel { delta, phase, .. })]
    MouseWheel {
        delta: MouseScrollDelta,
        phase: TouchPhase,
    },
}

/// The platform codes `platform` returns.
const APPKIT: i32 = 1;
const WIN32: i32 = 2;
const XLIB: i32 = 3;
const WAYLAND: i32 = 4;

/// Opened once, in `resumed`, because winit will not make a window before it.
struct App {
    attributes: WindowAttributes,
    window: Option<Window>,
    events: VecDeque<Event>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = event_loop.create_window(self.attributes.clone()).ok();
        }
    }

    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::window::WindowId,
        event: NativeWindowEvent,
    ) {
        let event = Event::from(event);
        if !matches!(event, Event::None) {
            self.events.push_back(event);
        }
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
        let mut windows = windows.borrow_mut();
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
    with(handle, Event::None, |open| {
        // Drain queued events before pumping again so none are discarded.
        if open.app.events.is_empty() {
            open.event_loop
                .pump_app_events(Some(Duration::ZERO), &mut open.app);
        }
        open.app.events.pop_front().unwrap_or(Event::None)
    })
    .into()
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

    // Todo: Implement caribou_abi::Buffer to Vec<u8> conversion and handle errors properly.
    // pub extern "C" fn set_cursor_custom(this: &WindowHandle, rgba: caribou_abi::Buffer, width: u16, height: u16, hotspot_x: u16, hotspot_y: u16) {
    //     with(this.handle, (), |open| {
    //         if let Some(window) = open.app.window.as_ref() {
    //             if let Ok(cursor) = winit::window::CustomCursor::from_rgba(rgba.to_vec(), width, height, hotspot_x, hotspot_y) {
    //                 window.set_cursor(winit::window::CursorIcon::Custom(cursor));
    //             }
    //         }
    //     });
    // }

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
                window.set_min_inner_size(Some(winit::dpi::LogicalSize::new(width, height)));
            }
        });
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

    pub extern "C" fn open(this: &mut WindowBuilder) -> Box<WindowHandle> {
        let handle = open_with_attributes(this.attributes.clone());

        Box::new(WindowHandle { handle })
    }
}

caribou_abi::plugin! {
    name:"window";


    enum MouseButton;
    enum MouseElementState;
    enum MouseScrollDelta;
    enum TouchPhase;
    enum Event;

    class Size {
        fn width(&Size) -> i32;
        fn height(&Size) -> i32;
    }

    class MonitorHandle {
        fn name(&MonitorHandle) -> Text;
        fn size(&MonitorHandle) -> Box<Size>;
    }

    class WindowHandle {
        fn poll(&WindowHandle) -> Enum<Event>;
        fn width(&WindowHandle) -> i32;
        fn height(&WindowHandle) -> i32;
        fn platform(&WindowHandle) -> i32;
        fn raw(&WindowHandle, i32) -> i64;
        fn set_cursor_icon(&WindowHandle, Text);
        fn set_position(&WindowHandle, i32, i32);
        fn set_size(&WindowHandle, i32, i32);
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
        fn open(&mut WindowBuilder) -> Box<WindowHandle>;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{
        DeviceId, ElementState, MouseButton as NativeMouseButton, MouseScrollDelta as NativeScroll,
        TouchPhase as NativePhase,
    };

    #[test]
    fn maps_native_events_without_a_window_or_host_heap() {
        assert!(matches!(
            Event::from(NativeWindowEvent::CloseRequested),
            Event::Closed
        ));
        assert!(matches!(
            Event::from(NativeWindowEvent::Resized(winit::dpi::PhysicalSize::new(
                800, 600
            ))),
            Event::Resized {
                width: 800,
                height: 600
            }
        ));
        assert!(matches!(
            Event::from(NativeWindowEvent::Moved(winit::dpi::PhysicalPosition::new(
                -10, 20
            ))),
            Event::Moved { x: -10, y: 20 }
        ));
        let event = NativeWindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: NativeMouseButton::Other(7),
        };
        assert!(matches!(
            Event::from(event),
            Event::MouseInput {
                state: MouseElementState::Pressed,
                button: MouseButton::Other(7)
            }
        ));
        let event = NativeWindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: NativeScroll::PixelDelta(winit::dpi::PhysicalPosition::new(1.5, -2.5)),
            phase: NativePhase::Ended,
        };
        let Event::MouseWheel {
            delta: MouseScrollDelta::PixelDelta { x, y },
            phase: TouchPhase::Ended,
        } = Event::from(event)
        else {
            panic!("wrong wheel mapping")
        };
        assert_eq!((x, y), (1.5, -2.5));
        assert!(matches!(
            Event::from(NativeWindowEvent::RedrawRequested),
            Event::None
        ));
    }
}
