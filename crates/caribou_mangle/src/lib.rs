//! The symbol a published member links under, in one place: the core's
//! link plan (`caribou::link`) and the plugin macro that exports a
//! plugin's members both spell it here, so an AOT call and the definition
//! it reaches agree by construction. See `docs/architecture/linking.md`.

#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};

/// The symbol for a member: `caribou`, then the language, the module as
/// its language spells it, the class, the kind's letter with the member's
/// name, and the arity, each after a `_`, and each name as its length
/// and its text. The kind letter is `m` for a method, `g` a getter, `s` a
/// setter, `t` a static and `c` a constructor. A character that is not a
/// C identifier's is written as `_` and two hex digits, `_` itself
/// included, so no two names share a symbol and the separators stay
/// readable.
pub fn symbol(lang: &str, module: &str, class: &str, kind: char, name: &str, arity: usize) -> String {
    let mut out = String::from("caribou");
    for part in [lang, module, class] {
        out.push('_');
        segment(&mut out, part);
    }
    out.push('_');
    out.push(kind);
    segment(&mut out, name);
    out.push('_');
    out.push_str(&arity.to_string());
    out
}

fn segment(out: &mut String, text: &str) {
    let escaped = escape(text);
    out.push_str(&escaped.len().to_string());
    out.push_str(&escaped);
}

fn escape(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() {
            out.push(b as char);
        } else {
            out.push('_');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_is_spelled_with_its_length() {
        assert_eq!(
            symbol("wren", "bench/tally", "Tally", 'm', "add", 1),
            "caribou_4wren_13bench_2ftally_5Tally_m3add_1"
        );
        assert_eq!(
            symbol("math", "Math", "Math", 't', "hypot", 2),
            "caribou_4math_4Math_4Math_t5hypot_2"
        );
        assert_eq!(symbol("wren", "m", "C", 's', "hp", 1), "caribou_4wren_1m_1C_s2hp_1");
    }
}
