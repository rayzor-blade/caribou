//! The process-wide symbol table: every member name the protocol is asked
//! about, interned once. A symbol is an id and a name; what a runtime
//! keys its own tables by (HashLink's field hash, say) is that runtime's
//! adapter's to derive from the name.

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
}

/// The names by id, in chunks that are allocated once and never moved,
/// so a read takes no lock: an id is handed out only after its name is
/// written and `LEN` published past it.
const CHUNK: usize = 1024;
const CHUNKS: usize = 4096;
static CHUNKS_BY_ID: [AtomicPtr<&'static str>; CHUNKS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; CHUNKS];
static LEN: AtomicUsize = AtomicUsize::new(0);

fn entry(id: u32) -> Option<&'static str> {
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
            let fresh: Box<[&'static str; CHUNK]> = Box::new([""; CHUNK]);
            chunk = Box::leak(fresh).as_mut_ptr();
            slot.store(chunk, Ordering::Release);
        }
        unsafe {
            chunk.add(id % CHUNK).write(name);
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
    entry(sym.0).unwrap_or("")
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
}
