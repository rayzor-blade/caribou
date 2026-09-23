//! Window and raw device events. Values remain Rust-owned in the queue;
//! encoding into Caribou's heap happens only when the caller polls.
use std::{cell::RefCell, collections::HashMap, path::PathBuf};

use caribou_abi::{Buffer, Enum, EnumField, ErrorKind, Kept, PluginEnum, TypeTag, Value, host};
use winit::{event as native, event_loop::AsyncRequestSerial, keyboard};

#[path = "events/keys.rs"]
mod keys;
pub use keys::{KeyCode, NamedKey};

thread_local! {
    // Winit deliberately keeps its IDs opaque. Intern by identity, never by
    // Debug output, hash value, pointer cast or platform-specific layout.
    static DEVICES: RefCell<HashMap<native::DeviceId, i32>> = RefCell::new(HashMap::new());
    static REQUESTS: RefCell<Requests> = const { RefCell::new(Requests { next: 1, pending: Vec::new() }) };
}

struct Requests {
    next: i64,
    pending: Vec<(AsyncRequestSerial, i64)>,
}

pub(crate) fn device_key(id: native::DeviceId) -> i32 {
    DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        let next = i32::try_from(devices.len() + 1).expect("device ID space exhausted");
        *devices.entry(id).or_insert(next)
    })
}

pub(crate) fn request_key(serial: AsyncRequestSerial) -> i64 {
    REQUESTS.with(|requests| {
        let mut requests = requests.borrow_mut();
        if let Some((_, id)) = requests.pending.iter().find(|(s, _)| *s == serial) {
            return *id;
        }
        let id = requests.next;
        requests.next += 1;
        requests.pending.push((serial, id));
        id
    })
}

fn activation_key(serial: AsyncRequestSerial) -> i64 {
    let id = request_key(serial);
    REQUESTS.with(|requests| {
        requests
            .borrow_mut()
            .pending
            .retain(|(_, value)| *value != id)
    });
    id
}

/// Owned bytes need no host allocation until the surrounding event is encoded.
#[derive(Debug, Clone, PartialEq)]
pub struct EventBytes(pub Vec<u8>);
impl EnumField for EventBytes {
    const TAG: TypeTag = TypeTag::BUFFER;
    fn into_value(self) -> Value {
        Buffer::new(&self.0).value()
    }
    fn from_value(value: Value) -> Self {
        Self(Buffer::of(value).unwrap().to_vec())
    }
}

/// Non-Unicode filenames retain their exact OS representation.
#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.FilePath")]
pub enum FilePath {
    Utf8(#[caribou(name = "path")] String),
    UnixBytes(#[caribou(name = "bytes")] EventBytes),
    WindowsWide(#[caribou(name = "utf16le")] EventBytes),
}
impl From<PathBuf> for FilePath {
    fn from(path: PathBuf) -> Self {
        match path.into_os_string().into_string() {
            Ok(path) => Self::Utf8(path),
            Err(path) => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt;
                    Self::UnixBytes(EventBytes(path.into_vec()))
                }
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStrExt;
                    Self::WindowsWide(EventBytes(
                        path.encode_wide().flat_map(u16::to_le_bytes).collect(),
                    ))
                }
                #[cfg(not(any(unix, windows)))]
                {
                    // The supported pump-events targets are Unix and Windows.
                    Self::Utf8(path.to_string_lossy().into_owned())
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.OptionalText")]
pub enum OptionalText {
    None,
    Some(#[caribou(name = "text")] String),
}
impl<T: ToString> From<Option<T>> for OptionalText {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::None, |s| Self::Some(s.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.OptionalFloat", from = Option::<f64>)]
pub enum OptionalFloat {
    None,
    Some(#[caribou(name = "value")] f64),
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.CursorRange")]
pub enum CursorRange {
    None,
    Range { start: i64, end: i64 },
}
impl From<Option<(usize, usize)>> for CursorRange {
    fn from(value: Option<(usize, usize)>) -> Self {
        value.map_or(Self::None, |(start, end)| Self::Range {
            start: start as i64,
            end: end as i64,
        })
    }
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.Ime", from = native::Ime)]
pub enum Ime {
    Enabled,
    Preedit(
        #[caribou(name = "text")] String,
        #[caribou(name = "cursor")] CursorRange,
    ),
    Commit(#[caribou(name = "text")] String),
    Disabled,
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.MouseButton", from = native::MouseButton)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(#[caribou(name = "button")] u16),
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.MouseElementState", from = native::ElementState)]
pub enum MouseElementState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.MouseScrollDelta", from = native::MouseScrollDelta)]
pub enum MouseScrollDelta {
    LineDelta(#[caribou(name = "x")] f32, #[caribou(name = "y")] f32),
    #[caribou(pattern = native::MouseScrollDelta::PixelDelta(position))]
    PixelDelta {
        #[caribou(value = position.x)]
        x: f64,
        #[caribou(value = position.y)]
        y: f64,
    },
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.TouchPhase", from = native::TouchPhase)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.TouchForce", from = native::Force)]
pub enum TouchForce {
    #[caribou(skip)]
    None,
    Calibrated {
        force: f64,
        max_possible_force: f64,
        altitude_angle: OptionalFloat,
    },
    Normalized(#[caribou(name = "force")] f64),
}
impl From<Option<native::Force>> for TouchForce {
    fn from(value: Option<native::Force>) -> Self {
        value.map_or(Self::None, Self::from)
    }
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.Theme", from = winit::window::Theme)]
pub enum Theme {
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.NativeKeyCode", from = keyboard::NativeKeyCode)]
pub enum NativeKeyCode {
    Unidentified,
    Android(#[caribou(name = "code")] i64),
    MacOS(#[caribou(name = "code")] u16),
    Windows(#[caribou(name = "code")] u16),
    Xkb(#[caribou(name = "code")] i64),
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.NativeKey", from = keyboard::NativeKey)]
pub enum NativeKey {
    Unidentified,
    Android(#[caribou(name = "code")] i64),
    MacOS(#[caribou(name = "code")] u16),
    Windows(#[caribou(name = "code")] u16),
    Xkb(#[caribou(name = "code")] i64),
    Web(#[caribou(name = "key", value = a0.to_string())] String),
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.PhysicalKey", from = keyboard::PhysicalKey)]
pub enum PhysicalKey {
    Code(#[caribou(name = "code")] KeyCode),
    Unidentified(#[caribou(name = "code")] NativeKeyCode),
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.Key", from = keyboard::Key)]
pub enum Key {
    Named(#[caribou(name = "key")] NamedKey),
    Character(#[caribou(name = "text", value = a0.to_string())] String),
    Unidentified(#[caribou(name = "key")] NativeKey),
    Dead(#[caribou(name = "character")] OptionalText),
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.KeyLocation", from = keyboard::KeyLocation)]
pub enum KeyLocation {
    Standard,
    Left,
    Right,
    Numpad,
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.KeySupplement")]
pub enum KeySupplement {
    Unavailable,
    Supplement {
        key_without_modifiers: Key,
        text_with_all_modifiers: OptionalText,
    },
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.KeyEvent")]
pub enum KeyEvent {
    Input {
        physical_key: PhysicalKey,
        logical_key: Key,
        text: OptionalText,
        location: KeyLocation,
        state: MouseElementState,
        repeat: bool,
        supplement: KeySupplement,
    },
}
impl From<native::KeyEvent> for KeyEvent {
    fn from(event: native::KeyEvent) -> Self {
        #[cfg(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "redox"
        ))]
        let supplement = {
            use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
            KeySupplement::Supplement {
                key_without_modifiers: event.key_without_modifiers().into(),
                text_with_all_modifiers: event.text_with_all_modifiers().into(),
            }
        };
        #[cfg(not(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "redox"
        )))]
        let supplement = KeySupplement::Unavailable;
        Self::Input {
            physical_key: event.physical_key.into(),
            logical_key: event.logical_key.into(),
            text: event.text.into(),
            location: event.location.into(),
            state: event.state.into(),
            repeat: event.repeat,
            supplement,
        }
    }
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.ModifiersKeyState", from = keyboard::ModifiersKeyState)]
pub enum ModifiersKeyState {
    Unknown,
    Pressed,
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.Modifiers")]
pub enum Modifiers {
    State {
        shift: bool,
        control: bool,
        alt: bool,
        super_key: bool,
        left_shift: ModifiersKeyState,
        right_shift: ModifiersKeyState,
        left_control: ModifiersKeyState,
        right_control: ModifiersKeyState,
        left_alt: ModifiersKeyState,
        right_alt: ModifiersKeyState,
        left_super: ModifiersKeyState,
        right_super: ModifiersKeyState,
    },
}
impl From<native::Modifiers> for Modifiers {
    fn from(m: native::Modifiers) -> Self {
        Self::State {
            shift: m.state().shift_key(),
            control: m.state().control_key(),
            alt: m.state().alt_key(),
            super_key: m.state().super_key(),
            left_shift: m.lshift_state().into(),
            right_shift: m.rshift_state().into(),
            left_control: m.lcontrol_state().into(),
            right_control: m.rcontrol_state().into(),
            left_alt: m.lalt_state().into(),
            right_alt: m.ralt_state().into(),
            left_super: m.lsuper_state().into(),
            right_super: m.rsuper_state().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.DeviceEvent", from = native::DeviceEvent)]
pub enum DeviceEvent {
    Added,
    Removed,
    #[caribou(pattern = native::DeviceEvent::MouseMotion { delta })]
    MouseMotion {
        #[caribou(value = delta.0)]
        x: f64,
        #[caribou(value = delta.1)]
        y: f64,
    },
    MouseWheel {
        delta: MouseScrollDelta,
    },
    Motion {
        axis: i64,
        value: f64,
    },
    Button {
        button: i64,
        state: MouseElementState,
    },
    #[caribou(pattern = native::DeviceEvent::Key(event))]
    Key {
        #[caribou(value = event.physical_key.into())]
        physical_key: PhysicalKey,
        #[caribou(value = event.state.into())]
        state: MouseElementState,
    },
}

/// Return this from the synchronous scale callback. The writer never escapes
/// winit's callback; Default preserves winit's suggested size.
#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.ScaleSize")]
pub enum ScaleSize {
    Default,
    Physical { width: i32, height: i32 },
}

// No fallback: upgrading winit with an additional WindowEvent must fail to
// compile until its payload has an explicit representation here.
#[derive(Debug, Clone, PartialEq, PluginEnum)]
#[caribou(name = "window.Event", from = native::WindowEvent)]
pub enum Event {
    #[caribou(skip)]
    None,
    #[caribou(pattern = native::WindowEvent::CloseRequested)]
    Closed,
    #[caribou(pattern = native::WindowEvent::Resized(size))]
    Resized {
        #[caribou(value = size.width as i64)]
        width: i64,
        #[caribou(value = size.height as i64)]
        height: i64,
    },
    #[caribou(pattern = native::WindowEvent::Moved(position))]
    Moved {
        #[caribou(value = position.x)]
        x: i32,
        #[caribou(value = position.y)]
        y: i32,
    },
    CursorEntered {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
    },
    CursorLeft {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
    },
    #[caribou(pattern = native::WindowEvent::CursorMoved { device_id, position })]
    CursorMoved {
        #[caribou(value = position.x)]
        x: f64,
        #[caribou(value = position.y)]
        y: f64,
        #[caribou(value = device_key(device_id))]
        device_id: i32,
    },
    MouseInput {
        state: MouseElementState,
        button: MouseButton,
        #[caribou(value = device_key(device_id))]
        device_id: i32,
    },
    MouseWheel {
        delta: MouseScrollDelta,
        phase: TouchPhase,
        #[caribou(value = device_key(device_id))]
        device_id: i32,
    },
    ActivationTokenDone {
        #[caribou(value = activation_key(serial))]
        serial: i64,
        #[caribou(value = token.into_raw())]
        token: String,
    },
    Destroyed,
    DroppedFile(#[caribou(name = "path")] FilePath),
    HoveredFile(#[caribou(name = "path")] FilePath),
    HoveredFileCancelled,
    Focused(#[caribou(name = "focused")] bool),
    KeyboardInput {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
        event: KeyEvent,
        is_synthetic: bool,
    },
    ModifiersChanged(#[caribou(name = "modifiers")] Modifiers),
    Ime(#[caribou(name = "event")] Ime),
    PinchGesture {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
        delta: f64,
        phase: TouchPhase,
    },
    #[caribou(pattern = native::WindowEvent::PanGesture { device_id, delta, phase })]
    PanGesture {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
        #[caribou(value = delta.x)]
        x: f32,
        #[caribou(value = delta.y)]
        y: f32,
        phase: TouchPhase,
    },
    DoubleTapGesture {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
    },
    RotationGesture {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
        delta: f32,
        phase: TouchPhase,
    },
    TouchpadPressure {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
        pressure: f32,
        stage: i64,
    },
    AxisMotion {
        #[caribou(value = device_key(device_id))]
        device_id: i32,
        axis: i64,
        value: f64,
    },
    #[caribou(pattern = native::WindowEvent::Touch(touch))]
    Touch {
        #[caribou(value = device_key(touch.device_id))]
        device_id: i32,
        #[caribou(value = touch.phase.into())]
        phase: TouchPhase,
        #[caribou(value = touch.location.x)]
        x: f64,
        #[caribou(value = touch.location.y)]
        y: f64,
        #[caribou(value = touch.force.into())]
        force: TouchForce,
        // Preserve all 64 ID bits; Haxe sees an Int64, not a float.
        #[caribou(value = touch.id as i64)]
        id: i64,
    },
    #[caribou(pattern = native::WindowEvent::ScaleFactorChanged { scale_factor, .. })]
    ScaleFactorChanged {
        scale_factor: f64,
    },
    ThemeChanged(#[caribou(name = "theme")] Theme),
    Occluded(#[caribou(name = "occluded")] bool),
    RedrawRequested,
    #[caribou(skip)]
    Device {
        device_id: i32,
        event: DeviceEvent,
    },
    #[caribou(skip)]
    Resumed,
    #[caribou(skip)]
    Suspended,
    #[caribou(skip)]
    MemoryWarning,
}

pub(crate) enum CallbackError {
    Raised(Kept),
    Invalid(&'static str),
}
impl CallbackError {
    pub(crate) fn raise(self) {
        match self {
            Self::Raised(error) => host::raise_value(error.get()),
            Self::Invalid(message) => host::raise(ErrorKind::Type, message),
        }
    }
}

pub(crate) fn scale_request(callback: &Kept, factor: f64) -> Result<ScaleSize, CallbackError> {
    let value = host::call(callback.get(), &[Value::number(factor)])
        .map_err(|e| CallbackError::Raised(Kept::new(e)))?;
    let size = Enum::<ScaleSize>::of(value)
        .ok_or(CallbackError::Invalid(
            "scale callback must return window.ScaleSize",
        ))?
        .get();
    if matches!(size, ScaleSize::Physical { width, height } if width < 0 || height < 0) {
        return Err(CallbackError::Invalid(
            "scale callback dimensions must be nonnegative",
        ));
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::{PhysicalPosition, PhysicalSize};

    #[test]
    fn all_constructible_window_events_are_preserved_without_a_heap() {
        use native::WindowEvent as W;
        let device_id = native::DeviceId::dummy();
        let events = [
            W::Resized(PhysicalSize::new(u32::MAX, 600)),
            W::Moved((-10, 20).into()),
            W::CloseRequested,
            W::Destroyed,
            W::DroppedFile("a.txt".into()),
            W::HoveredFile("b.txt".into()),
            W::HoveredFileCancelled,
            W::Focused(true),
            W::ModifiersChanged(keyboard::ModifiersState::SHIFT.into()),
            W::Ime(native::Ime::Enabled),
            W::CursorMoved {
                device_id,
                position: (1.5, -2.5).into(),
            },
            W::CursorEntered { device_id },
            W::CursorLeft { device_id },
            W::MouseInput {
                device_id,
                state: native::ElementState::Pressed,
                button: native::MouseButton::Other(65535),
            },
            W::MouseWheel {
                device_id,
                delta: native::MouseScrollDelta::PixelDelta((1.0, -1.0).into()),
                phase: native::TouchPhase::Ended,
            },
            W::PinchGesture {
                device_id,
                delta: 0.25,
                phase: native::TouchPhase::Started,
            },
            W::PanGesture {
                device_id,
                delta: PhysicalPosition::new(2.0, 3.0),
                phase: native::TouchPhase::Moved,
            },
            W::DoubleTapGesture { device_id },
            W::RotationGesture {
                device_id,
                delta: -45.0,
                phase: native::TouchPhase::Cancelled,
            },
            W::TouchpadPressure {
                device_id,
                pressure: 0.7,
                stage: i64::MAX,
            },
            W::AxisMotion {
                device_id,
                axis: u32::MAX,
                value: 1.5,
            },
            W::Touch(native::Touch {
                device_id,
                phase: native::TouchPhase::Moved,
                location: (3.0, 4.0).into(),
                force: Some(native::Force::Calibrated {
                    force: 2.0,
                    max_possible_force: 4.0,
                    altitude_angle: None,
                }),
                id: u64::MAX,
            }),
            W::ThemeChanged(winit::window::Theme::Dark),
            W::Occluded(true),
            W::RedrawRequested,
        ];
        assert_eq!(events.len(), 25);
        for native in events {
            let event = Event::from(native);
            assert!(!matches!(event, Event::None));
        }
        // The other three native types contain private winit state: keyboard,
        // activation serial and size writer. The generated From match is
        // exhaustive, and their public payload schemas are checked below.
        assert_eq!(Event::DESC.variant_count, 33); // 28 window + idle/device/lifecycle
        let variants =
            unsafe { std::slice::from_raw_parts(Event::DESC.variants, Event::DESC.variant_count) };
        for (name, fields) in [
            ("KeyboardInput", 3),
            ("ActivationTokenDone", 2),
            ("ScaleFactorChanged", 1),
        ] {
            let variant = variants
                .iter()
                .find(|v| unsafe { v.name.as_str() } == name)
                .unwrap();
            assert_eq!(variant.field_count, fields);
        }
    }

    #[test]
    fn identities_and_numeric_payloads_are_not_truncated() {
        let device_id = native::DeviceId::dummy();
        let id = device_key(device_id);
        assert_eq!(
            Event::from(native::WindowEvent::CursorEntered { device_id }),
            Event::CursorEntered { device_id: id }
        );
        assert_eq!(
            Event::from(native::WindowEvent::CursorLeft { device_id }),
            Event::CursorLeft { device_id: id }
        );
        assert_eq!(
            Event::from(native::WindowEvent::Resized(PhysicalSize::new(
                u32::MAX,
                600
            ))),
            Event::Resized {
                width: u32::MAX as i64,
                height: 600
            }
        );
        assert_eq!(
            NativeKeyCode::from(keyboard::NativeKeyCode::Xkb(u32::MAX)),
            NativeKeyCode::Xkb(u32::MAX as i64)
        );
        let event = Event::from(native::WindowEvent::Touch(native::Touch {
            device_id,
            phase: native::TouchPhase::Ended,
            location: (1.5, -2.5).into(),
            force: None,
            id: u64::MAX,
        }));
        assert!(matches!(
            event,
            Event::Touch {
                id: -1,
                force: TouchForce::None,
                x: 1.5,
                y: -2.5,
                ..
            }
        ));
    }

    #[test]
    fn keyboard_ime_and_force_keep_optional_data() {
        assert_eq!(
            Key::from(keyboard::Key::Dead(Some('é'))),
            Key::Dead(OptionalText::Some("é".into()))
        );
        assert_eq!(
            Key::from(keyboard::Key::Dead(None)),
            Key::Dead(OptionalText::None)
        );
        assert_eq!(
            Key::from(keyboard::Key::Named(keyboard::NamedKey::F35)),
            Key::Named(NamedKey::F35)
        );
        assert_eq!(
            PhysicalKey::from(keyboard::PhysicalKey::Code(keyboard::KeyCode::KeyA)),
            PhysicalKey::Code(KeyCode::KeyA)
        );
        assert_eq!(
            Ime::from(native::Ime::Preedit("é文".into(), Some((2, 5)))),
            Ime::Preedit("é文".into(), CursorRange::Range { start: 2, end: 5 })
        );
        assert_eq!(
            TouchForce::from(Some(native::Force::Normalized(0.75))),
            TouchForce::Normalized(0.75)
        );
    }

    #[test]
    fn raw_device_events_preserve_native_keys_and_axes() {
        let events = [
            native::DeviceEvent::Added,
            native::DeviceEvent::Removed,
            native::DeviceEvent::MouseMotion {
                delta: (1.25, -2.5),
            },
            native::DeviceEvent::MouseWheel {
                delta: native::MouseScrollDelta::LineDelta(1.0, 2.0),
            },
            native::DeviceEvent::Motion {
                axis: u32::MAX,
                value: 0.5,
            },
            native::DeviceEvent::Button {
                button: u32::MAX,
                state: native::ElementState::Released,
            },
            native::DeviceEvent::Key(native::RawKeyEvent {
                physical_key: keyboard::PhysicalKey::Unidentified(
                    keyboard::NativeKeyCode::Android(u32::MAX),
                ),
                state: native::ElementState::Pressed,
            }),
        ];
        let converted: Vec<_> = events.into_iter().map(DeviceEvent::from).collect();
        assert_eq!(converted[2], DeviceEvent::MouseMotion { x: 1.25, y: -2.5 });
        assert_eq!(
            converted[4],
            DeviceEvent::Motion {
                axis: u32::MAX as i64,
                value: 0.5
            }
        );
        assert_eq!(
            converted[6],
            DeviceEvent::Key {
                physical_key: PhysicalKey::Unidentified(NativeKeyCode::Android(u32::MAX as i64)),
                state: MouseElementState::Pressed
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_paths_keep_the_original_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = std::ffi::OsString::from_vec(vec![b'/', 255]);
        assert_eq!(
            FilePath::from(PathBuf::from(path)),
            FilePath::UnixBytes(EventBytes(vec![b'/', 255]))
        );
    }
}
