use caribou_abi::{EnumField, PluginEnum, TypeTag, Value};

enum Native {
    Idle,
    Tuple(i32, bool),
    Position { x: i32, y: i32 },
    Ignored,
}

#[derive(Debug, PartialEq, PluginEnum)]
#[caribou(name = "test.Event", from = Native, fallback = Self::Empty)]
enum Event {
    #[caribou(skip)]
    Empty,
    Idle,
    Tuple(#[caribou(name = "count")] i32, bool),
    #[caribou(name = "Moved", pattern = Native::Position { x, y })]
    Point {
        #[caribou(value = x + y)]
        sum: i32,
    },
}

#[derive(PluginEnum)]
#[caribou(name = "test.Nested")]
enum Nested {
    Pair { left: Event, right: Event },
    Ref(Value),
}

#[test]
fn generates_native_mappings_and_complete_schema() {
    assert_eq!(Event::from(Native::Idle), Event::Idle);
    assert_eq!(Event::from(Native::Tuple(7, true)), Event::Tuple(7, true));
    assert_eq!(
        Event::from(Native::Position { x: 2, y: 3 }),
        Event::Point { sum: 5 }
    );
    assert_eq!(Event::from(Native::Ignored), Event::Empty);
    let desc = Event::DESC;
    assert_eq!(unsafe { desc.name.as_str() }, "test.Event");
    assert_eq!(desc.variant_count, 4);
    let variants = unsafe { std::slice::from_raw_parts(desc.variants, desc.variant_count) };
    assert_eq!(variants[0].field_count, 0);
    assert_eq!(unsafe { (*variants[2].fields).name.as_str() }, "count");
    assert_eq!(unsafe { (*variants[2].fields.add(1)).tag }, TypeTag::BOOL);
    assert_eq!(unsafe { variants[3].name.as_str() }, "Moved");
    assert_eq!(unsafe { (*variants[3].fields).name.as_str() }, "sum");
    let nested = unsafe { &*Nested::DESC.variants };
    assert_eq!(unsafe { (*nested.fields).tag }, TypeTag::ENUM);
    assert!(std::ptr::eq(
        unsafe { (*nested.fields).enumeration },
        Event::DESC
    ));
}

#[test]
fn visits_only_existing_host_references() {
    let mut refs = Vec::new();
    Nested::Pair {
        left: Event::Idle,
        right: Event::Point { sum: 7 },
    }
    .visit(&mut |v| refs.push(v));
    assert!(refs.is_empty());
    let value = Value::object(0x1000 as *const _);
    Nested::Ref(value).visit(&mut |v| refs.push(v));
    assert_eq!(refs, [value]);
}
