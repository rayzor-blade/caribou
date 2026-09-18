//! The `math` plugin of the interop tests: free functions, which land on
//! a class named after the plugin; a class of statics; and a class whose
//! instances cross, over scalars and one value passed as it is.

use std::sync::atomic::{AtomicI32, Ordering};

use caribou_abi::Value;

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
}

impl Drop for Vec2 {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

caribou_abi::plugin! {
    name: "math";
    fn hypot(f64, f64) -> f64;
    fn twice(i32) -> i32;
    fn is_even(i64) -> bool;
    fn bump() -> i32;
    fn same(Value) -> Value;
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
    }
}
