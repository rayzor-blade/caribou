//! Headless probes of the window plugin's events: xwindow's generated
//! `window` model and native backend, with a class of samples that makes
//! the events a window reports without opening one.
#![allow(non_snake_case)]
// Nearly all of the crate is xwindow's generated model and backend, which
// xwindow lints.
#![allow(clippy::all)]

mod backend {
    include!(concat!(env!("OUT_DIR"), "/xwindow_backend/native.rs"));

    use super::*;

    pub unsafe fn samples_event(which: i32) -> Event {
        sample(which)
    }

    /// As the backend returns an enum: its native code.
    pub unsafe fn samples_sizing(which: i32) -> i32 {
        match which {
            0 => ScaleSizing::Logical,
            _ => ScaleSizing::Physical,
        }
        .native()
    }

    /// The theme's native code, or -1 for none.
    pub unsafe fn samples_theme(theme: Option<i32>) -> i32 {
        theme.unwrap_or(-1)
    }
}
mod runtime {
    pub use caribou_abi::{Buffer, BufferMut, Enum, ErrorKind, Future, Text, host};
}

#[allow(unused_imports)]
use runtime::{Buffer, BufferMut, Enum, Future, Text};
include!(concat!(env!("OUT_DIR"), "/window.rs"));

fn sample(which: i32) -> Event {
    let device_id = 7;
    match which {
        // A width no i32 holds.
        0 => Event::Resized {
            width: i64::from(u32::MAX),
            height: 600,
        },
        // The cursor range is in bytes.
        1 => Event::Ime {
            event: Ime::Preedit {
                text: "é文".into(),
                cursor: CursorRange::Range { start: 2, end: 5 },
            },
        },
        // Every bit of a 64-bit touch id.
        2 => Event::Touch {
            device_id,
            phase: TouchPhase::Moved,
            x: 1.5,
            y: -2.5,
            force: TouchForce::Calibrated {
                force: 2.0,
                max_possible_force: 4.0,
                altitude_angle: OptionalFloat::Some { value: 0.5 },
            },
            id: u64::MAX as i64,
        },
        3 => Event::KeyboardInput {
            device_id,
            event: KeyEvent::Input {
                physical_key: PhysicalKey::Code {
                    code: KeyCode::KeyA,
                },
                logical_key: Key::Character { text: "é".into() },
                text: OptionalText::Some { text: "é".into() },
                location: KeyLocation::Left,
                state: MouseElementState::Pressed,
                repeat: true,
                supplement: KeySupplement::Supplement {
                    key_without_modifiers: Key::Named {
                        key: NamedKey::Enter,
                    },
                    text_with_all_modifiers: OptionalText::None,
                },
            },
            is_synthetic: true,
        },
        4 => Event::MouseInput {
            state: MouseElementState::Released,
            button: MouseButton::Other { button: 65535 },
            device_id,
        },
        // A native key code past i32.
        5 => Event::Device {
            device_id,
            event: DeviceEvent::Key {
                physical_key: PhysicalKey::Unidentified {
                    code: NativeKeyCode::Xkb {
                        code: i64::from(u32::MAX),
                    },
                },
                state: MouseElementState::Pressed,
            },
        },
        // A path that is not Unicode keeps its bytes.
        6 => Event::DroppedFile {
            path: FilePath::UnixBytes {
                bytes: VariantBytes(vec![b'/', 255]),
            },
        },
        7 => Event::RedrawRequested,
        8 => Event::ModifiersChanged {
            modifiers: Modifiers::State {
                shift: true,
                control: false,
                alt: false,
                super_key: false,
                left_shift: ModifiersKeyState::Pressed,
                right_shift: ModifiersKeyState::Unknown,
                left_control: ModifiersKeyState::Unknown,
                right_control: ModifiersKeyState::Unknown,
                left_alt: ModifiersKeyState::Unknown,
                right_alt: ModifiersKeyState::Unknown,
                left_super: ModifiersKeyState::Unknown,
                right_super: ModifiersKeyState::Unknown,
            },
        },
        9 => Event::ThemeChanged { theme: Theme::Dark },
        // An id no double holds exactly.
        10 => Event::Touch {
            device_id,
            phase: TouchPhase::Started,
            x: 0.0,
            y: 0.0,
            force: TouchForce::None,
            id: (1 << 60) + 1,
        },
        _ => Event::None,
    }
}
