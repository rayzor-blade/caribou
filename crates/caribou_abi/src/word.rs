//! A plugin function's arguments and result as 64-bit words, the one shape
//! [`SymbolDesc::call`](crate::SymbolDesc::call) takes.
//!
//! Every type a plugin signature crosses is one word or less. A word holds
//! it in its low bytes: an integer as its bits, a float as its bits, a bool
//! as 0 or 1, a pointer as its address. Reading a type back is reading those
//! bytes, which is why this is defined for little-endian targets alone.

#[cfg(not(target_endian = "little"))]
compile_error!(
    "caribou_abi words hold a value in their low bytes, which needs a little-endian target"
);

/// A plugin function as the core calls it: its argument words in order, its
/// result word back (0 for none).
pub type Call = unsafe extern "C" fn(args: *const u64) -> u64;

/// The next argument, as type `T`, and `args` past it.
///
/// # Safety
/// `*args` is a word holding a valid `T`.
#[inline(always)]
pub unsafe fn take<T>(args: &mut *const u64) -> T {
    const { assert!(core::mem::size_of::<T>() <= 8) };
    unsafe {
        let word = args.read();
        *args = args.add(1);
        (&raw const word).cast::<T>().read_unaligned()
    }
}

/// `value` as a result word, its bytes moved into the word's low bytes.
#[inline(always)]
pub fn to_word<T>(value: T) -> u64 {
    const { assert!(core::mem::size_of::<T>() <= 8) };
    let mut word = 0u64;
    unsafe { (&raw mut word).cast::<T>().write_unaligned(value) };
    word
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_back_from_their_words() {
        let n = 7u8;
        let words = [
            to_word(-3i32),
            to_word(1.5f32),
            to_word(true),
            to_word(-9i64),
            to_word(2.25f64),
            to_word(&n as *const u8),
        ];
        let mut args = words.as_ptr();
        unsafe {
            assert_eq!(take::<i32>(&mut args), -3);
            assert_eq!(take::<f32>(&mut args), 1.5);
            assert!(take::<bool>(&mut args));
            assert_eq!(take::<i64>(&mut args), -9);
            assert_eq!(take::<f64>(&mut args), 2.25);
            assert_eq!(*take::<*const u8>(&mut args), 7);
        }
        // A narrow value leaves the rest of its word zero.
        assert_eq!(to_word(-1i32), u64::from(u32::MAX));
        assert_eq!(to_word(()), 0);
    }
}
