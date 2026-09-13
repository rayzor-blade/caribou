//! The process-wide symbol table: every member name the protocol is asked
//! about, interned once. A symbol carries HashLink's field hash so the same
//! name resolves to the same `hashed_name` here and in Ash.

use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// An interned name. One table per process; the core assigns ids in
/// order of first use, and `Symbol(0)` is the empty string.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(pub u32);

impl Symbol {
    pub const EMPTY: Symbol = Symbol(0);

    pub fn intern(name: &str) -> Symbol {
        intern(name)
    }

    pub fn name(self) -> &'static str {
        name(self)
    }

    pub fn hash(self) -> i32 {
        hash(self)
    }
}

#[derive(Clone, Copy)]
struct Entry {
    name: &'static str,
    hash: i32,
}

/// The entries by id, in chunks that are allocated once and never moved,
/// so a read takes no lock: an id is handed out only after its entry is
/// written and `LEN` published past it.
const CHUNK: usize = 1024;
const CHUNKS: usize = 4096;
static CHUNKS_BY_ID: [AtomicPtr<Entry>; CHUNKS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; CHUNKS];
static LEN: AtomicUsize = AtomicUsize::new(0);

fn entry(id: u32) -> Option<Entry> {
    let id = id as usize;
    if id >= LEN.load(Ordering::Acquire) {
        return None;
    }
    let chunk = CHUNKS_BY_ID[id / CHUNK].load(Ordering::Acquire);
    Some(unsafe { *chunk.add(id % CHUNK) })
}

/// Writers: the names interned so far. Every write to the chunks happens
/// under this lock, before `LEN` is advanced.
struct Interner {
    by_name: HashMap<&'static str, u32>,
}

fn table() -> &'static Mutex<Interner> {
    static TABLE: OnceLock<Mutex<Interner>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut interner = Interner {
            by_name: HashMap::new(),
        };
        interner.insert("");
        Mutex::new(interner)
    })
}

impl Interner {
    fn insert(&mut self, name: &str) -> Symbol {
        if let Some(&id) = self.by_name.get(name) {
            return Symbol(id);
        }
        let id = LEN.load(Ordering::Relaxed);
        assert!(id < CHUNK * CHUNKS, "the symbol table is full");
        // Symbols live for the process, so the name is leaked once.
        let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
        let slot = &CHUNKS_BY_ID[id / CHUNK];
        let mut chunk = slot.load(Ordering::Acquire);
        if chunk.is_null() {
            let fresh: Box<[Entry; CHUNK]> = Box::new([Entry { name: "", hash: 0 }; CHUNK]);
            chunk = Box::leak(fresh).as_mut_ptr();
            slot.store(chunk, Ordering::Release);
        }
        unsafe {
            chunk.add(id % CHUNK).write(Entry {
                name,
                hash: hl_hash(name),
            });
        }
        LEN.store(id + 1, Ordering::Release);
        self.by_name.insert(name, id as u32);
        Symbol(id as u32)
    }
}

/// The symbol for `name`, created on first use.
pub fn intern(name: &str) -> Symbol {
    table().lock().unwrap().insert(name)
}

/// The symbol for `name` if it has been interned.
pub fn lookup(name: &str) -> Option<Symbol> {
    table()
        .lock()
        .unwrap()
        .by_name
        .get(name)
        .map(|&id| Symbol(id))
}

/// The name behind `sym`; empty for an id the table never issued.
pub fn name(sym: Symbol) -> &'static str {
    entry(sym.0).map_or("", |e| e.name)
}

/// HashLink's field hash of the symbol's name: what `hl_obj_field::
/// hashed_name` holds for it in Ash.
pub fn hash(sym: Symbol) -> i32 {
    entry(sym.0).map_or(0, |e| e.hash)
}

/// HashLink's `hl_hash_gen` over the UTF-16 encoding of `name`, stopping at
/// a NUL as the C loop does: `h = 223 * h + unit` in wrapping 32-bit
/// arithmetic, then a truncating remainder by `0x1FFFFF7B`. HashLink's own
/// table then probes upward on a collision between two live names; that
/// step depends on its cache and is not reproduced here.
pub fn hl_hash(name: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in name.encode_utf16().take_while(|&u| u != 0) {
        h = h.wrapping_mul(223).wrapping_add(unit as i32);
    }
    h.wrapping_rem(0x1FFF_FF7B)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_twice_gives_one_symbol_and_the_name_round_trips() {
        let a = intern("caribou_symbol_test_x");
        let b = intern("caribou_symbol_test_x");
        let c = intern("caribou_symbol_test_y");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(name(a), "caribou_symbol_test_x");
        assert_eq!(a.name(), "caribou_symbol_test_x");
        assert_eq!(lookup("caribou_symbol_test_x"), Some(a));
        assert_eq!(lookup("caribou_symbol_never_interned"), None);
        assert_eq!(Symbol::EMPTY.name(), "");
        assert_eq!(intern(""), Symbol::EMPTY);
        assert_eq!(name(Symbol(u32::MAX)), "");
    }

    /// Expected values follow Ash's `hlp_hash_gen` step by step: `h = 223 h
    /// + unit`, then `h % 0x1FFFFF7B` truncating toward zero.
    #[test]
    fn hash_matches_hashlinks_field_hash() {
        // One unit: h = 'x' = 120, under the modulus.
        assert_eq!(hl_hash("x"), 120);
        assert_eq!(intern("x").hash(), 120);
        // "ab": 223 * 97 + 98 = 21729.
        assert_eq!(hl_hash("ab"), 21_729);
        // Long enough to wrap i32; the remainder keeps the sign of `h`.
        assert_eq!(hl_hash("length"), -16_280_745);
        assert_eq!(hl_hash("toString"), 409_915_697);
        assert_eq!(hl_hash("__constructor__"), 483_945_737);
        // Non-ASCII hashes by UTF-16 unit, not by UTF-8 byte.
        assert_eq!(hl_hash("é"), 233);
        assert_eq!(hl_hash("\u{1F600}"), 223 * 0xD83D + 0xDE00);
        // A NUL ends the name, as it does in C.
        assert_eq!(hl_hash("ab\0cd"), hl_hash("ab"));
        assert_eq!(hl_hash(""), 0);
        assert_eq!(hash(Symbol(u32::MAX)), 0);
    }
}
