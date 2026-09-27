//! The window in a page: the page's canvas, as Ash's page reports it through
//! a block of shared memory (Ash's `docs/wasm/window.md`, the `ash_window`
//! crate). `poll` reads the events the page wrote; the rest queue commands,
//! which the page carries out at its next frame. What a page cannot do, such
//! as place or decorate its window, does nothing.

use std::cell::RefCell;
use std::collections::VecDeque;

use ash_window::{Command, Event as PageEvent, Window, pointer};
use caribou_abi::*;

use crate::WindowLevel;
use crate::events::*;

/// The platform code `platform` returns: a page's canvas, which is what the
/// GPU plugin's web surface draws on (the native codes are 1 to 4).
const WEB: i32 = 5;

/// Ring sizes: room for a burst of pointer events between two polls, and
/// for a frame's commands.
const EVENT_RING: u32 = 1 << 16;
const COMMAND_RING: u32 = 1 << 12;

/// What a window opens with; what a page cannot honour is not kept.
#[derive(Clone, Default)]
pub(crate) struct Attributes {
    title: Option<String>,
    size: Option<(i32, i32)>,
    min_size: Option<(i32, i32)>,
    max_size: Option<(i32, i32)>,
    fullscreen: bool,
    visible: Option<bool>,
}

impl Attributes {
    pub(crate) fn title(self, title: &str) -> Self {
        Self {
            title: Some(title.to_owned()),
            ..self
        }
    }
    pub(crate) fn size(self, width: i32, height: i32) -> Self {
        Self {
            size: Some((width, height)),
            ..self
        }
    }
    pub(crate) fn fullscreen(self, yes: bool) -> Self {
        Self {
            fullscreen: yes,
            ..self
        }
    }
    pub(crate) fn resizable(self, _: bool) -> Self {
        self
    }
    pub(crate) fn maximized(self, _: bool) -> Self {
        self
    }
    pub(crate) fn visible(self, yes: bool) -> Self {
        Self {
            visible: Some(yes),
            ..self
        }
    }
    pub(crate) fn content_protected(self, _: bool) -> Self {
        self
    }
    pub(crate) fn decorations(self, _: bool) -> Self {
        self
    }
    pub(crate) fn transparent(self, _: bool) -> Self {
        self
    }
    pub(crate) fn blur(self, _: bool) -> Self {
        self
    }
    pub(crate) fn min_size(self, width: i32, height: i32) -> Self {
        Self {
            min_size: Some((width, height)),
            ..self
        }
    }
    pub(crate) fn max_size(self, width: i32, height: i32) -> Self {
        Self {
            max_size: Some((width, height)),
            ..self
        }
    }
    pub(crate) fn window_level(self, _: WindowLevel) -> Self {
        self
    }
}

struct Open {
    window: Window,
    /// The block, kept for as long as the page may write it: the program's.
    _block: Box<[u64]>,
    events: VecDeque<Event>,
    scale_callback: Option<Kept>,
    pending_error: Option<CallbackError>,
    /// A custom cursor's pixels, which the page reads when it drains the
    /// command, so they stay until the next one replaces them.
    cursor: Vec<u8>,
    monitor: (String, u32, u32),
    closed: bool,
}

thread_local! {
    /// A handle is an index into this, plus one, so zero is never a window.
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
        match usize::try_from(handle - 1)
            .ok()
            .and_then(|i| windows.get_mut(i))
            .and_then(Option::as_mut)
        {
            Some(open) => body(open),
            None => miss,
        }
    })
}

fn send(handle: i32, command: Command<'_>) {
    with(handle, (), |open| {
        open.window.send(command);
    });
}

/// The page's canvas as a window: zero when the program's host has no page.
pub(crate) fn open(attributes: Attributes) -> i32 {
    let words = Window::block_size(EVENT_RING, COMMAND_RING).div_ceil(8);
    let mut block = vec![0u64; words].into_boxed_slice();
    let window = unsafe { Window::init(block.as_mut_ptr().cast(), EVENT_RING, COMMAND_RING) };
    if !host::agent("window", window.address() as usize) {
        return 0;
    }
    if let Some(title) = &attributes.title {
        window.send(Command::SetTitle(title));
    }
    if let Some((width, height)) = attributes.size {
        window.send(Command::SetSize {
            width: width.max(0) as f64,
            height: height.max(0) as f64,
        });
    }
    if let Some((width, height)) = attributes.min_size {
        window.send(Command::SetMinSize {
            width: width.max(0) as f64,
            height: height.max(0) as f64,
        });
    }
    if let Some((width, height)) = attributes.max_size {
        window.send(Command::SetMaxSize {
            width: width.max(0) as f64,
            height: height.max(0) as f64,
        });
    }
    if let Some(visible) = attributes.visible {
        window.send(Command::SetVisible(visible));
    }
    if attributes.fullscreen {
        window.send(Command::Fullscreen(true));
    }
    window.send(Command::RequestMonitor);
    let open = Open {
        window,
        _block: block,
        events: VecDeque::new(),
        scale_callback: None,
        pending_error: None,
        cursor: Vec::new(),
        monitor: (String::new(), 0, 0),
        closed: false,
    };
    WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        windows.push(Some(open));
        windows.len() as i32
    })
}

/// The next event, reading what the page wrote once the queue is empty.
pub(crate) fn poll(handle: i32) -> Enum<Event> {
    let (event, error) = with(handle, (Event::None, None), |open| {
        if open.closed {
            return (Event::None, None);
        }
        if open.events.is_empty() {
            read(open);
        }
        match open.pending_error.take() {
            Some(error) => (Event::None, Some(error)),
            None => (open.events.pop_front().unwrap_or(Event::None), None),
        }
    });
    if let Some(error) = error {
        error.raise();
    }
    event.into()
}

/// Everything the page wrote, as the plugin's events.
fn read(open: &mut Open) {
    let Open {
        window,
        events,
        scale_callback,
        pending_error,
        monitor,
        ..
    } = open;
    let mut resize = None;
    window.poll(|event| match event {
        PageEvent::Resized { width, height, .. } => events.push_back(Event::Resized {
            width: i64::from(width),
            height: i64::from(height),
        }),
        PageEvent::Focused(focused) => events.push_back(Event::Focused(focused)),
        PageEvent::Occluded(hidden) => events.push_back(Event::Occluded(hidden)),
        PageEvent::Close => events.push_back(Event::Closed),
        PageEvent::PointerEntered { id, pointer_type } if pointer_type != pointer::TOUCH => {
            events.push_back(Event::CursorEntered { device_id: id })
        }
        PageEvent::PointerLeft { id, pointer_type } if pointer_type != pointer::TOUCH => {
            events.push_back(Event::CursorLeft { device_id: id })
        }
        PageEvent::PointerMoved {
            id,
            pointer_type,
            x,
            y,
            pressure,
            ..
        } => events.push_back(if pointer_type == pointer::TOUCH {
            touch(id, TouchPhase::Moved, x, y, pressure)
        } else {
            Event::CursorMoved {
                x,
                y,
                device_id: id,
            }
        }),
        PageEvent::PointerButton {
            id,
            pointer_type,
            pressed,
            button,
            x,
            y,
            pressure,
        } => events.push_back(if pointer_type == pointer::TOUCH {
            let phase = if pressed {
                TouchPhase::Started
            } else {
                TouchPhase::Ended
            };
            touch(id, phase, x, y, pressure)
        } else {
            Event::MouseInput {
                state: state(pressed),
                button: mouse_button(button),
                device_id: id,
            }
        }),
        PageEvent::PointerCancelled { id, pointer_type } if pointer_type == pointer::TOUCH => {
            events.push_back(touch(id, TouchPhase::Cancelled, 0.0, 0.0, 0.0))
        }
        // A browser's delta points the way the page scrolls; winit's, the
        // way the content moves.
        PageEvent::Wheel { mode, dx, dy, .. } => events.push_back(Event::MouseWheel {
            delta: if mode == 0 {
                MouseScrollDelta::PixelDelta { x: -dx, y: -dy }
            } else {
                MouseScrollDelta::LineDelta(-dx as f32, -dy as f32)
            },
            phase: TouchPhase::Moved,
            device_id: 0,
        }),
        PageEvent::Key {
            pressed,
            location,
            flags,
            code,
            key,
            ..
        } => events.push_back(Event::KeyboardInput {
            device_id: 0,
            event: key_event(pressed, location, flags, code, key),
            is_synthetic: flags & ash_window::key_flags::SYNTHETIC != 0,
        }),
        PageEvent::Modifiers { modifiers, sides } => {
            events.push_back(Event::ModifiersChanged(key_modifiers(modifiers, sides)))
        }
        PageEvent::Theme { dark } => events.push_back(Event::ThemeChanged(if dark {
            Theme::Dark
        } else {
            Theme::Light
        })),
        PageEvent::Redraw => events.push_back(Event::RedrawRequested),
        PageEvent::ScaleFactor(scale_factor) => {
            if let Some(callback) = scale_callback.as_ref()
                && pending_error.is_none()
            {
                match scale_request(callback, scale_factor) {
                    Ok(ScaleSize::Physical { width, height }) => {
                        resize = Some((
                            f64::from(width) / scale_factor,
                            f64::from(height) / scale_factor,
                        ))
                    }
                    Ok(ScaleSize::Default) => {}
                    Err(error) => *pending_error = Some(error),
                }
            }
            events.push_back(Event::ScaleFactorChanged { scale_factor });
        }
        PageEvent::Ime {
            what,
            start,
            end,
            text,
        } => events.push_back(Event::Ime(match what {
            ash_window::ime::ENABLED => Ime::Enabled,
            ash_window::ime::PREEDIT => Ime::Preedit(
                text.to_owned(),
                if start >= 0 && end >= 0 {
                    CursorRange::Range {
                        start: i64::from(start),
                        end: i64::from(end),
                    }
                } else {
                    CursorRange::None
                },
            ),
            ash_window::ime::COMMIT => Ime::Commit(text.to_owned()),
            _ => Ime::Disabled,
        })),
        // A page learns a file's name only when it is dropped, and never
        // its path.
        PageEvent::FileHovered { .. } => {
            events.push_back(Event::HoveredFile(FilePath::Utf8(String::new())))
        }
        PageEvent::FileHoverCancelled => events.push_back(Event::HoveredFileCancelled),
        PageEvent::FileDropped { name, .. } => {
            events.push_back(Event::DroppedFile(FilePath::Utf8(name.to_owned())))
        }
        PageEvent::Pinch { phase, delta, .. } => events.push_back(Event::PinchGesture {
            device_id: 0,
            delta,
            phase: touch_phase(phase),
        }),
        PageEvent::TouchpadPressure { pressure, stage } => {
            events.push_back(Event::TouchpadPressure {
                device_id: 0,
                pressure,
                stage: i64::from(stage),
            })
        }
        PageEvent::MouseMotion { dx, dy } => events.push_back(Event::Device {
            device_id: 0,
            event: DeviceEvent::MouseMotion { x: dx, y: dy },
        }),
        PageEvent::Suspended => events.push_back(Event::Suspended),
        PageEvent::Resumed => events.push_back(Event::Resumed),
        PageEvent::Monitor {
            width,
            height,
            label,
            ..
        } => *monitor = (label.to_owned(), width, height),
        _ => {}
    });
    if let Some((width, height)) = resize {
        window.send(Command::SetSize { width, height });
    }
}

fn touch(id: i32, phase: TouchPhase, x: f64, y: f64, pressure: f32) -> Event {
    Event::Touch {
        device_id: id,
        phase,
        x,
        y,
        force: TouchForce::Normalized(f64::from(pressure)),
        id: i64::from(id),
    }
}

fn touch_phase(phase: u32) -> TouchPhase {
    match phase {
        ash_window::phase::STARTED => TouchPhase::Started,
        ash_window::phase::ENDED => TouchPhase::Ended,
        ash_window::phase::CANCELLED => TouchPhase::Cancelled,
        _ => TouchPhase::Moved,
    }
}

fn state(pressed: bool) -> MouseElementState {
    if pressed {
        MouseElementState::Pressed
    } else {
        MouseElementState::Released
    }
}

fn mouse_button(button: u32) -> MouseButton {
    match button {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::Back,
        4 => MouseButton::Forward,
        other => MouseButton::Other(other.min(u32::from(u16::MAX)) as u16),
    }
}

/// A key as the page reports it: `code` names the physical key and `key`
/// the logical one, both by the W3C names winit's variants carry.
fn key_event(pressed: bool, location: u32, flags: u32, code: &str, key: &str) -> KeyEvent {
    let physical_key = match KeyCode::from_name(code) {
        KeyCode::Unrecognized => PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        code => PhysicalKey::Code(code),
    };
    let logical_key = match key {
        "Dead" => Key::Dead(OptionalText::None),
        "Unidentified" => Key::Unidentified(NativeKey::Web(code.to_owned())),
        " " => Key::Named(NamedKey::Space),
        _ => match NamedKey::from_name(key) {
            NamedKey::Unrecognized if key.chars().count() == 1 => Key::Character(key.to_owned()),
            NamedKey::Unrecognized => Key::Unidentified(NativeKey::Web(key.to_owned())),
            named => Key::Named(named),
        },
    };
    let text = match &logical_key {
        Key::Character(text) if pressed => OptionalText::Some(text.clone()),
        Key::Named(NamedKey::Space) if pressed => OptionalText::Some(" ".to_owned()),
        _ => OptionalText::None,
    };
    KeyEvent::Input {
        physical_key,
        logical_key,
        text,
        location: match location {
            1 => KeyLocation::Left,
            2 => KeyLocation::Right,
            3 => KeyLocation::Numpad,
            _ => KeyLocation::Standard,
        },
        state: state(pressed),
        repeat: flags & ash_window::key_flags::REPEAT != 0,
        supplement: KeySupplement::Unavailable,
    }
}

fn key_modifiers(modifiers: u32, sides: u32) -> Modifiers {
    use ash_window::{modifiers as m, sides as s};
    let side = |bit: u32| {
        if sides & bit != 0 {
            ModifiersKeyState::Pressed
        } else {
            ModifiersKeyState::Unknown
        }
    };
    Modifiers::State {
        shift: modifiers & m::SHIFT != 0,
        control: modifiers & m::CONTROL != 0,
        alt: modifiers & m::ALT != 0,
        super_key: modifiers & m::META != 0,
        left_shift: side(s::LEFT_SHIFT),
        right_shift: side(s::RIGHT_SHIFT),
        left_control: side(s::LEFT_CONTROL),
        right_control: side(s::RIGHT_CONTROL),
        left_alt: side(s::LEFT_ALT),
        right_alt: side(s::RIGHT_ALT),
        left_super: side(s::LEFT_META),
        right_super: side(s::RIGHT_META),
    }
}

pub(crate) fn width(handle: i32) -> i32 {
    with(handle, 0, |open| open.window.size().0 as i32)
}

pub(crate) fn height(handle: i32) -> i32 {
    with(handle, 0, |open| open.window.size().1 as i32)
}

pub(crate) fn platform(handle: i32) -> i32 {
    with(handle, 0, |_| WEB)
}

/// A page's canvas has no native handle to give.
pub(crate) fn raw(_: i32, _: i32) -> i64 {
    0
}

pub(crate) fn scale_factor(handle: i32) -> f64 {
    with(handle, 1.0, |open| {
        let scale = open.window.scale_factor();
        if scale > 0.0 { scale } else { 1.0 }
    })
}

pub(crate) fn monitor_name(handle: i32) -> String {
    with(handle, String::new(), |open| open.monitor.0.clone())
}

/// The screen in physical pixels.
pub(crate) fn monitor_size(handle: i32) -> (i32, i32) {
    let scale = scale_factor(handle);
    with(handle, (0, 0), |open| {
        let (width, height) = match open.monitor {
            (_, 0, 0) => open.window.screen_size(),
            (_, width, height) => (width, height),
        };
        (
            (f64::from(width) * scale) as i32,
            (f64::from(height) * scale) as i32,
        )
    })
}

/// A CSS cursor keyword, as winit's cursor names are.
pub(crate) fn set_cursor_icon(handle: i32, icon: &str) {
    send(handle, Command::SetCursor(icon));
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
        open.cursor = rgba;
        open.window.send(Command::SetCursorImage {
            address: open.cursor.as_ptr() as usize as u32,
            width: u32::from(width),
            height: u32::from(height),
            hot_x: u32::from(hotspot_x),
            hot_y: u32::from(hotspot_y),
        });
    });
}

pub(crate) fn set_position(_: i32, _: i32, _: i32) {}

pub(crate) fn set_size(handle: i32, width: i32, height: i32) {
    send(
        handle,
        Command::SetSize {
            width: width.max(0) as f64,
            height: height.max(0) as f64,
        },
    );
}

pub(crate) fn request_redraw(handle: i32) {
    send(handle, Command::RequestRedraw);
}

pub(crate) fn set_ime_allowed(handle: i32, allowed: bool) {
    send(handle, Command::SetImeAllowed(allowed));
}

/// A logical rectangle; the page takes physical pixels.
pub(crate) fn set_ime_cursor_area(handle: i32, x: f64, y: f64, width: f64, height: f64) {
    let scale = scale_factor(handle);
    send(
        handle,
        Command::SetImeArea {
            x: x * scale,
            y: y * scale,
            width: width * scale,
            height: height * scale,
        },
    );
}

pub(crate) fn on_scale_factor_changed(handle: i32, callback: Option<Kept>) {
    with(handle, (), |open| open.scale_callback = callback);
}

pub(crate) fn request_activation_token(_: i32) -> i64 {
    0
}

pub(crate) fn set_fullscreen(handle: i32, yes: bool) {
    send(handle, Command::Fullscreen(yes));
}

pub(crate) fn has_focus(handle: i32) -> bool {
    with(handle, false, |open| open.window.has_focus())
}

pub(crate) fn focus(handle: i32) {
    send(handle, Command::Focus);
}

pub(crate) fn set_blur(_: i32, _: bool) {}

/// The page's screen: `handle` itself stands for it.
pub(crate) fn current_monitor(handle: i32) -> i32 {
    with(handle, 0, |_| handle)
}

/// A page's canvas cannot close; it is hidden, and polls nothing more.
pub(crate) fn close(handle: i32) {
    with(handle, (), |open| {
        open.window.send(Command::SetVisible(false));
        open.scale_callback = None;
        open.pending_error = None;
        open.closed = true;
    });
}
