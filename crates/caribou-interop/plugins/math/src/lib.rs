//! The `math` plugin of the interop tests: free functions, which land on
//! a class named after the plugin; a class of statics; a class whose
//! instances cross, over scalars and one value passed as it is; strings
//! both ways; a class that keeps a function it was given and calls it;
//! and errors raised to the caller.

use std::sync::atomic::{AtomicI32, Ordering};

use caribou_abi::{Buffer, Enum, ErrorKind, Kept, Text, Value, host};

pub extern "C" fn hypot(a: f64, b: f64) -> f64 {
    a.hypot(b)
}

pub extern "C" fn twice(n: i32) -> i32 {
    n * 2
}

pub extern "C" fn is_even(n: i64) -> bool {
    n % 2 == 0
}

static BUMPS: AtomicI32 = AtomicI32::new(0);

pub extern "C" fn bump() -> i32 {
    BUMPS.fetch_add(1, Ordering::Relaxed) + 1
}

pub extern "C" fn same(v: Value) -> Value {
    v
}

pub extern "C" fn shout(s: Text) -> Text {
    Text::new(&format!("{}!", s.to_uppercase()))
}

pub extern "C" fn width(s: Text) -> i32 {
    s.chars().count() as i32
}

/// A quotient, or an error the caller sees.
pub extern "C" fn quotient(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        host::raise(ErrorKind::Arithmetic, "quotient by zero");
        return 0.0;
    }
    a / b
}

pub struct Vec;

impl Vec {
    pub extern "C" fn len3(x: f64, y: f64, z: f64) -> f64 {
        (x * x + y * y + z * z).sqrt()
    }
}

/// How many `Vec2` the plugin has out: what the core has not dropped yet.
static LIVE: AtomicI32 = AtomicI32::new(0);

pub struct Vec2 {
    x: f64,
    y: f64,
}

impl Vec2 {
    pub extern "C" fn new(x: f64, y: f64) -> Box<Vec2> {
        LIVE.fetch_add(1, Ordering::Relaxed);
        Box::new(Vec2 { x, y })
    }

    pub extern "C" fn len(this: &Vec2) -> f64 {
        this.x.hypot(this.y)
    }

    pub extern "C" fn scale(this: &mut Vec2, k: f64) {
        this.x *= k;
        this.y *= k;
    }

    pub extern "C" fn dot(this: &Vec2, other: &Vec2) -> f64 {
        this.x * other.x + this.y * other.y
    }

    pub extern "C" fn unit(this: &Vec2) -> Box<Vec2> {
        let n = Self::len(this);
        Self::new(this.x / n, this.y / n)
    }

    pub extern "C" fn live() -> i32 {
        LIVE.load(Ordering::Relaxed)
    }

    pub extern "C" fn tally(this: &Vec2) -> Box<Tally> {
        Box::new(Tally {
            total: Self::len(this),
            on_step: None,
        })
    }
}

impl Drop for Vec2 {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A running total that tells a function it was given about each step.
pub struct Tally {
    total: f64,
    on_step: Option<Kept>,
}

impl Tally {
    pub extern "C" fn new() -> Box<Tally> {
        Box::new(Tally {
            total: 0.0,
            on_step: None,
        })
    }

    pub extern "C" fn watch(this: &mut Tally, f: Value) {
        this.on_step = Some(Kept::new(f));
    }

    /// The new total, as the watcher answered it, or as it is when there
    /// is none. An error the watcher raises is the caller's.
    pub extern "C" fn add(this: &mut Tally, n: f64) -> f64 {
        this.total += n;
        if let Some(f) = &this.on_step {
            match host::call(f.get(), &[Value::number(this.total)]) {
                Ok(v) => this.total = v.as_number().unwrap_or(this.total),
                Err(e) => host::raise_value(e),
            }
        }
        this.total
    }

    pub extern "C" fn label(this: &Tally, name: Text) -> Text {
        Text::new(&format!("{name}: {}", this.total))
    }
}

/// Data conversion probes used by the Haxe and Wren fixtures.
pub struct Data;
thread_local! { static SAVED: std::cell::RefCell<Option<Kept>> = const { std::cell::RefCell::new(None) }; }
impl Data {
    pub extern "C" fn vector() -> Box<Vec2> {
        Vec2::new(3.0, 4.0)
    }
    pub extern "C" fn bytes() -> Buffer {
        Buffer::new(&[0, 128, 255, 65])
    }
    pub extern "C" fn same_storage(a: Buffer, b: Buffer) -> bool {
        a.as_ptr() == b.as_ptr()
    }
    pub extern "C" fn empty() -> Buffer {
        Buffer::new(&[])
    }
    pub extern "C" fn echo(bytes: Buffer) -> Buffer {
        bytes
    }
    pub extern "C" fn sum(bytes: Buffer) -> i32 {
        unsafe { bytes.as_slice() }.iter().map(|b| *b as i32).sum()
    }
    pub extern "C" fn save(bytes: Buffer) {
        SAVED.with(|v| *v.borrow_mut() = Some(Kept::new(bytes.value())));
    }
    pub extern "C" fn saved() -> Buffer {
        SAVED.with(|v| Buffer::of(v.borrow().as_ref().unwrap().get()).unwrap())
    }
    pub extern "C" fn event(which: i32) -> Enum<Event> {
        match which {
            0 => Event::Closed.into(),
            1 => Event::Resized(800, 600).into(),
            3 => Event::Wide(4_294_967_298).into(),
            _ => {
                let label = Text::new("héllo");
                let root = Kept::new(label.value());
                let bytes = Buffer::new(&[0, 255]);
                let value = Event::Message(label, bytes, true, 1.5).into();
                drop(root);
                value
            }
        }
    }
    pub extern "C" fn echo_event(event: Enum<Event>) -> Enum<Event> {
        event
    }
    pub extern "C" fn area(event: Enum<Event>) -> i32 {
        match event.get() {
            Event::Resized(w, h) => w * h,
            Event::Wide(n) => (n >> 32) as i32,
            _ => -1,
        }
    }
    pub extern "C" fn nested(event: Enum<Event>) -> Enum<Nested> {
        Nested::Event(event).into()
    }
}

caribou_abi::plugin! {
    name: "math";
    enum Event {
        Closed;
        Resized(width: i32, height: i32);
        Message(label: Text, bytes: Buffer, enabled: bool, ratio: f64);
        Wide(value: i64);
    }
    enum Nested { Event(event: Enum<Event>); }
    class Data {
        fn vector() -> Box<Vec2>;
        fn bytes() -> Buffer;
        fn same_storage(Buffer, Buffer) -> bool;
        fn empty() -> Buffer;
        fn echo(Buffer) -> Buffer;
        fn sum(Buffer) -> i32;
        fn save(Buffer);
        fn saved() -> Buffer;
        fn event(i32) -> Enum<Event>;
        fn echo_event(Enum<Event>) -> Enum<Event>;
        fn area(Enum<Event>) -> i32;
        fn nested(Enum<Event>) -> Enum<Nested>;
    }
    fn hypot(f64, f64) -> f64;
    fn twice(i32) -> i32;
    fn is_even(i64) -> bool;
    fn bump() -> i32;
    fn same(Value) -> Value;
    fn shout(Text) -> Text;
    fn width(Text) -> i32;
    fn quotient(f64, f64) -> f64;
    class Vec {
        fn len3(f64, f64, f64) -> f64;
    }
    class Vec2 {
        fn new(f64, f64) -> Box<Vec2>;
        fn len(&Vec2) -> f64;
        fn scale(&mut Vec2, f64);
        fn dot(&Vec2, &Vec2) -> f64;
        fn unit(&Vec2) -> Box<Vec2>;
        fn live() -> i32;
        fn tally(&Vec2) -> Box<Tally>;
    }
    class Tally {
        fn new() -> Box<Tally>;
        fn watch(&mut Tally, Value);
        fn add(&mut Tally, f64) -> f64;
        fn label(&Tally, Text) -> Text;
    }
}
