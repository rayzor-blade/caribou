//! The bundle: one file carrying a program and the modules of every
//! language it uses, each in its language's own form. What `caribou
//! build` writes from a project's layout and `caribou run` opens in place
//! of the program and its source roots.
//!
//! The manifest names the bundle, its entry module, the one the driver
//! starts, and the namespaces the project's layout gave. Then come the
//! sections: a module is a language name, the format of its bytes as the
//! language versions it (`hl`, `source`, `wlbc@7`), the module's name in
//! its language (`game/hud`) and the bytes. The bundle versions its own
//! framing only; a module's format is its language's to read and to
//! refuse. A source is the text a compiled module was built from, for
//! its language's diagnostics, under the module's name. A resource is
//! bytes by name, for whoever asks for it. A native library is one a run
//! opens from a file, under its file name, for the target its format
//! names (`aarch64-macos`): a plugin on the shared ABI, or a runtime's
//! own plugin form (a Zyntax `.zrtl`, with `lang` naming the runtime);
//! a run takes the ones of its own target. A language is a frontend the
//! run brings up itself to read the bundle's modules, under its name:
//! the format says how it comes, a Zyntax snapshot with its grammar
//! (`zsnap`) or one this build of caribou has in it (`builtin`).
//!
//! Wire format, all integers little-endian:
//!
//! ```text
//! magic         "CARIBOU\0"     8 bytes
//! version       u32
//! flags         u32             none defined; a set bit is refused
//! manifest      name, entry language, entry module, namespaces
//! section_count u32
//! sections      kind u8, language, format, name, data
//! ```
//!
//! A string or a byte string is its length as a `u32` and the bytes; a
//! list is its count as a `u32` and the items; a namespace is its name,
//! its languages and, after a `u8` that says whether there are any, its
//! modules.

use std::fmt;

use crate::registry::Namespace;

pub const MAGIC: [u8; 8] = *b"CARIBOU\0";
pub const VERSION: u32 = 1;

/// The target a native library section is for, as this build names its
/// own: `<arch>-<os>` from the standard library's constants.
pub fn target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// What a section holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SectionKind {
    Module = 1,
    Resource = 2,
    Source = 3,
    NativeLib = 4,
    Language = 5,
}

/// One section: for a module, `lang` names the language, `format` the
/// form of `data` as the language versions it, `name` the module in its
/// language; for a source, `lang` and the module's `name`, `format`
/// empty, `data` the text; for a resource, `name` alone, `lang` and
/// `format` empty; for a native library, `format` the target, `name`
/// the file name, `lang` the runtime whose plugin form it is, or empty
/// for a plugin on the shared ABI; for a language, `lang` its name,
/// `format` how it comes (`zsnap`, `builtin`), `name` the file it came
/// from or empty, `data` the snapshot or empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    pub lang: String,
    pub format: String,
    pub name: String,
    pub data: Vec<u8>,
}

/// The module the driver starts: its language and its module name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub lang: String,
    pub module: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub entry: Entry,
    pub namespaces: Vec<Namespace>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub manifest: Manifest,
    pub sections: Vec<Section>,
}

impl Bundle {
    /// The sections holding modules.
    pub fn modules(&self) -> impl Iterator<Item = &Section> {
        self.sections
            .iter()
            .filter(|s| s.kind == SectionKind::Module)
    }

    /// The native libraries for `target`.
    pub fn native_libs<'a>(&'a self, target: &'a str) -> impl Iterator<Item = &'a Section> {
        self.sections
            .iter()
            .filter(move |s| s.kind == SectionKind::NativeLib && s.format == target)
    }

    /// The languages a run brings up to read the modules.
    pub fn languages(&self) -> impl Iterator<Item = &Section> {
        self.sections
            .iter()
            .filter(|s| s.kind == SectionKind::Language)
    }

    /// The entry module's section, when the bundle carries it.
    pub fn entry(&self) -> Option<&Section> {
        let entry = &self.manifest.entry;
        self.modules()
            .find(|s| s.lang == entry.lang && s.name == entry.module)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotABundle,
    Version(u32),
    Flags(u32),
    Truncated,
    Text,
    Kind(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotABundle => write!(f, "not a caribou bundle"),
            Error::Version(v) => write!(f, "bundle version {v}; this build reads {VERSION}"),
            Error::Flags(bits) => write!(f, "bundle flags {bits:#x} are not known here"),
            Error::Truncated => write!(f, "the bundle ends early"),
            Error::Text => write!(f, "a bundle string is not UTF-8"),
            Error::Kind(k) => write!(f, "section kind {k} is not known here"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether `bytes` start as a bundle does.
pub fn looks_like(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

/// The bundle as bytes.
pub fn emit(bundle: &Bundle) -> Vec<u8> {
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(&MAGIC);
    w.u32(VERSION);
    w.u32(0);
    let m = &bundle.manifest;
    w.str(&m.name);
    w.str(&m.entry.lang);
    w.str(&m.entry.module);
    w.u32(m.namespaces.len() as u32);
    for ns in &m.namespaces {
        w.str(&ns.name);
        w.u32(ns.langs.len() as u32);
        for lang in &ns.langs {
            w.str(lang);
        }
        match &ns.modules {
            None => w.0.push(0),
            Some(modules) => {
                w.0.push(1);
                w.u32(modules.len() as u32);
                for module in modules {
                    w.str(module);
                }
            }
        }
    }
    w.u32(bundle.sections.len() as u32);
    for s in &bundle.sections {
        w.0.push(s.kind as u8);
        w.str(&s.lang);
        w.str(&s.format);
        w.str(&s.name);
        w.bytes(&s.data);
    }
    w.0
}

/// The bundle in `bytes`.
pub fn load(bytes: &[u8]) -> Result<Bundle, Error> {
    if !looks_like(bytes) {
        return Err(Error::NotABundle);
    }
    let mut r = Reader(&bytes[MAGIC.len()..]);
    let version = r.u32()?;
    if version != VERSION {
        return Err(Error::Version(version));
    }
    let flags = r.u32()?;
    if flags != 0 {
        return Err(Error::Flags(flags));
    }
    let name = r.str()?;
    let entry = Entry {
        lang: r.str()?,
        module: r.str()?,
    };
    let mut namespaces = Vec::new();
    for _ in 0..r.u32()? {
        let name = r.str()?;
        let mut langs = Vec::new();
        for _ in 0..r.u32()? {
            langs.push(r.str()?);
        }
        let modules = if r.u8()? == 0 {
            None
        } else {
            let mut modules = Vec::new();
            for _ in 0..r.u32()? {
                modules.push(r.str()?);
            }
            Some(modules)
        };
        namespaces.push(Namespace {
            name,
            langs,
            modules,
        });
    }
    let mut sections = Vec::new();
    for _ in 0..r.u32()? {
        let kind = match r.u8()? {
            1 => SectionKind::Module,
            2 => SectionKind::Resource,
            3 => SectionKind::Source,
            4 => SectionKind::NativeLib,
            5 => SectionKind::Language,
            k => return Err(Error::Kind(k)),
        };
        sections.push(Section {
            kind,
            lang: r.str()?,
            format: r.str()?,
            name: r.str()?,
            data: r.bytes()?.to_vec(),
        });
    }
    Ok(Bundle {
        manifest: Manifest {
            name,
            entry,
            namespaces,
        },
        sections,
    })
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, n: u32) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }

    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn str(&mut self) -> Result<String, Error> {
        std::str::from_utf8(self.bytes()?)
            .map(str::to_owned)
            .map_err(|_| Error::Text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bundle {
        Bundle {
            manifest: Manifest {
                name: "hud".to_owned(),
                entry: Entry {
                    lang: "haxe".to_owned(),
                    module: "hud".to_owned(),
                },
                namespaces: vec![
                    Namespace {
                        name: "game".to_owned(),
                        langs: vec!["haxe".to_owned(), "wren".to_owned()],
                        modules: None,
                    },
                    Namespace {
                        name: "ui".to_owned(),
                        langs: vec!["wren".to_owned()],
                        modules: Some(vec!["hud".to_owned()]),
                    },
                ],
            },
            sections: vec![
                Section {
                    kind: SectionKind::Module,
                    lang: "haxe".to_owned(),
                    format: "hl".to_owned(),
                    name: "hud".to_owned(),
                    data: b"HLB\x05...".to_vec(),
                },
                Section {
                    kind: SectionKind::Module,
                    lang: "wren".to_owned(),
                    format: "source".to_owned(),
                    name: "game/hud".to_owned(),
                    data: b"class Hud {}\n".to_vec(),
                },
                Section {
                    kind: SectionKind::Source,
                    lang: "wren".to_owned(),
                    format: String::new(),
                    name: "game/hud".to_owned(),
                    data: b"class Hud {}\n".to_vec(),
                },
                Section {
                    kind: SectionKind::Resource,
                    lang: String::new(),
                    format: String::new(),
                    name: "font.ttf".to_owned(),
                    data: vec![0, 1, 2],
                },
                Section {
                    kind: SectionKind::NativeLib,
                    lang: String::new(),
                    format: "aarch64-macos".to_owned(),
                    name: "libmath.dylib".to_owned(),
                    data: vec![0xcf, 0xfa, 0xed, 0xfe],
                },
                Section {
                    kind: SectionKind::Language,
                    lang: "zynml".to_owned(),
                    format: "zsnap".to_owned(),
                    name: "zynml.zsnap".to_owned(),
                    data: b"ZSNP...".to_vec(),
                },
                Section {
                    kind: SectionKind::Language,
                    lang: "python".to_owned(),
                    format: "builtin".to_owned(),
                    name: String::new(),
                    data: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn a_bundle_survives_the_round_trip() {
        let bundle = sample();
        let bytes = emit(&bundle);
        assert!(looks_like(&bytes));
        let back = load(&bytes).expect("loads");
        assert_eq!(back, bundle);
        assert_eq!(back.entry().map(|s| s.name.as_str()), Some("hud"));
        assert_eq!(back.modules().count(), 2);
        assert_eq!(back.native_libs("aarch64-macos").count(), 1);
        assert_eq!(back.native_libs("x86_64-windows").count(), 0);
        let languages: Vec<(&str, &str)> = back
            .languages()
            .map(|s| (s.lang.as_str(), s.format.as_str()))
            .collect();
        assert_eq!(languages, [("zynml", "zsnap"), ("python", "builtin")]);
    }

    #[test]
    fn what_is_not_a_bundle_is_refused() {
        assert_eq!(load(b"HLB").unwrap_err(), Error::NotABundle);
        let bytes = emit(&sample());
        assert_eq!(load(&bytes[..20]).unwrap_err(), Error::Truncated);
        let mut other = bytes.clone();
        other[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(load(&other).unwrap_err(), Error::Version(99));
        let mut flagged = bytes.clone();
        flagged[12] = 1;
        assert_eq!(load(&flagged).unwrap_err(), Error::Flags(1));
    }
}
