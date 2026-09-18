//! The `math` plugin of the interop tests: free functions, which land on
//! a class named after the plugin, and a class of its own, over scalars
//! and one value passed as it is.

use std::sync::atomic::{AtomicI32, Ordering};

use caribou_abi::Value;

static BUMPS: AtomicI32 = AtomicI32::new(0);

caribou_abi::plugin! {
    name: "math";

    fn hypot(a: f64, b: f64) -> f64 { a.hypot(b) }
    fn twice(n: i32) -> i32 { n * 2 }
    fn isEven(n: i64) -> bool { n % 2 == 0 }
    fn bump() -> i32 { BUMPS.fetch_add(1, Ordering::Relaxed) + 1 }
    fn same(v: Value) -> Value { v }

    class Vec {
        fn len3(x: f64, y: f64, z: f64) -> f64 { (x * x + y * y + z * z).sqrt() }
    }
}
