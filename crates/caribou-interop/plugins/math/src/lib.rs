//! The `math` plugin of the interop tests: free functions, which land on
//! a class named after the plugin, and a class of its own, over scalars
//! and one value passed as it is.

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
}
