//! Headless probes using the production window event definitions.
use caribou_abi::{Enum, Kept, Value};
#[path = "../../../../../plugins/cb_window/src/events.rs"]
mod events;
use events::*;

pub struct Samples;
impl Samples {
    pub extern "C" fn physical(width: i32, height: i32) -> Enum<ScaleSize> {
        ScaleSize::Physical { width, height }.into()
    }
    pub extern "C" fn event(which: i32) -> Enum<Event> {
        sample(which).into()
    }
    pub extern "C" fn echo(event: Enum<Event>) -> Enum<Event> {
        event.get().into()
    }
    pub extern "C" fn scale(callback: Value, factor: f64) -> Enum<ScaleSize> {
        match events::scale_request(&Kept::new(callback), factor) {
            Ok(size) => size.into(),
            Err(error) => {
                error.raise();
                ScaleSize::Default.into()
            }
        }
    }
}

caribou_abi::plugin! {
    name: "window";
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

    class Samples {
        fn physical(i32, i32) -> Enum<ScaleSize>;
        fn event(i32) -> Enum<Event>;
        fn echo(Enum<Event>) -> Enum<Event>;
        fn scale(Value, f64) -> Enum<ScaleSize>;
    }
}

fn sample(which: i32) -> Event {
    use winit::event as n;
    let device_id = n::DeviceId::dummy();
    match which {
        0 => n::WindowEvent::Resized(winit::dpi::PhysicalSize::new(u32::MAX, 600)).into(),
        1 => n::WindowEvent::Ime(n::Ime::Preedit("é文".into(), Some((2, 5)))).into(),
        2 => n::WindowEvent::Touch(n::Touch {
            device_id,
            phase: n::TouchPhase::Moved,
            location: (1.5, -2.5).into(),
            force: Some(n::Force::Calibrated {
                force: 2.0,
                max_possible_force: 4.0,
                altitude_angle: Some(0.5),
            }),
            id: u64::MAX,
        })
        .into(),
        3 => Event::KeyboardInput {
            device_id: events::device_key(device_id),
            event: KeyEvent::Input {
                physical_key: PhysicalKey::Code(KeyCode::KeyA),
                logical_key: Key::Character("é".into()),
                text: OptionalText::Some("é".into()),
                location: KeyLocation::Left,
                state: MouseElementState::Pressed,
                repeat: true,
                supplement: KeySupplement::Supplement {
                    key_without_modifiers: Key::Named(NamedKey::Enter),
                    text_with_all_modifiers: OptionalText::None,
                },
            },
            is_synthetic: true,
        },
        4 => n::WindowEvent::MouseInput {
            device_id,
            state: n::ElementState::Released,
            button: n::MouseButton::Other(65535),
        }
        .into(),
        5 => Event::Device {
            device_id: events::device_key(device_id),
            event: n::DeviceEvent::Key(n::RawKeyEvent {
                physical_key: winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Xkb(u32::MAX),
                ),
                state: n::ElementState::Pressed,
            })
            .into(),
        },
        6 => Event::DroppedFile(FilePath::UnixBytes(EventBytes(vec![b'/', 255]))),
        7 => n::WindowEvent::RedrawRequested.into(),
        8 => n::WindowEvent::ModifiersChanged(winit::keyboard::ModifiersState::SHIFT.into()).into(),
        9 => n::WindowEvent::ThemeChanged(winit::window::Theme::Dark).into(),
        // An id no double holds exactly.
        10 => n::WindowEvent::Touch(n::Touch {
            device_id,
            phase: n::TouchPhase::Started,
            location: (0.0, 0.0).into(),
            force: None,
            id: (1 << 60) + 1,
        })
        .into(),
        _ => Event::None,
    }
}
