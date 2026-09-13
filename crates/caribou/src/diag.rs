//! Diagnostics in two steps: [`report`] turns an `Error` value into a plain
//! [`Diagnostic`], the shape wren_lift and zyntax already print from, and
//! [`render`] draws one with ariadne. An adapter can take the `Diagnostic`
//! to its own renderer, or hand the core one of its own to draw beside the
//! core's. Nothing outside `render` names an ariadne type.

use std::borrow::{Borrow, Cow};
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::io::{self, Write};
use std::ops::Range;

use ariadne::{Cache, Color, Config, Fmt, IndexType, Label, Report, ReportKind, Source};
use caribou_abi::{ErrorKind, LangId, Value};

use crate::bridge::describe;
use crate::error::{Error, Str, kind_name};
use crate::world::language_name;

/// Resolves the source ids trace frames carry, a file path or module name,
/// to their text. Adapters implement it over their module tables.
pub trait SourceLookup {
    fn source_text(&self, id: &str) -> Option<Cow<'_, str>>;
}

impl<K, V, S> SourceLookup for HashMap<K, V, S>
where
    K: Borrow<str> + Hash + Eq,
    V: AsRef<str>,
    S: std::hash::BuildHasher,
{
    fn source_text(&self, id: &str) -> Option<Cow<'_, str>> {
        self.get(id).map(|text| Cow::Borrowed(text.as_ref()))
    }
}

/// Nothing resolves; every frame renders as a note.
pub struct NoSources;

impl SourceLookup for NoSources {
    fn source_text(&self, _id: &str) -> Option<Cow<'_, str>> {
        None
    }
}

/// A frame with a source span: one label in the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagLabel {
    pub lang: LangId,
    pub name: String,
    pub source: String,
    /// Byte offsets into the source.
    pub span: (usize, usize),
    pub message: String,
}

/// A plain description of an error: what [`render`] draws, and what an
/// adapter's own renderer can draw instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: ErrorKind,
    pub message: String,
    /// Frames that have a span, innermost first.
    pub labels: Vec<DiagLabel>,
    /// Frames without a span, in order, then anything else worth saying.
    pub notes: Vec<String>,
    pub cause: Option<Box<Diagnostic>>,
}

impl Diagnostic {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            kind,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            cause: None,
        }
    }

    pub fn with_label(mut self, label: DiagLabel) -> Diagnostic {
        self.labels.push(label);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    pub fn with_cause(mut self, cause: Diagnostic) -> Diagnostic {
        self.cause = Some(Box::new(cause));
        self
    }
}

/// Causes deeper than this are cut off; an error that is its own cause
/// would otherwise never end.
const MAX_CAUSE_DEPTH: usize = 16;

/// Describe `err`. A value that is not an `Error` is described as the
/// payload of a `User` error. An object `Value` must be a live object with
/// a `TypeDesc` at word zero, as everywhere in the bridge.
pub fn report(err: Value) -> Diagnostic {
    report_at(err, 0)
}

fn report_at(err: Value, depth: usize) -> Diagnostic {
    let Some(e) = (unsafe { Error::from_value(err) }) else {
        let message = unsafe { Str::text(err) }
            .map(str::to_owned)
            .unwrap_or_else(|| describe(err));
        return Diagnostic::new(ErrorKind::User, message);
    };
    let (kind, message, origin, native, cause) = unsafe {
        (
            Error::kind(e),
            Error::message_str(e).to_owned(),
            Error::origin(e),
            Error::native(e),
            Error::cause(e),
        )
    };
    let mut diag = Diagnostic::new(kind, message);
    for (i, frame) in unsafe { Error::frames(e) }.iter().enumerate() {
        let name = unsafe { frame.name_str() };
        let name = if name.is_empty() { "<callable>" } else { name };
        let lang = language_name(frame.lang);
        match (unsafe { frame.source_str() }, frame.span()) {
            (Some(source), Some(span)) => diag.labels.push(DiagLabel {
                lang: frame.lang,
                name: name.to_owned(),
                source: source.to_owned(),
                span,
                message: if i == 0 {
                    format!("raised in {name} ({lang})")
                } else {
                    format!("through {name} ({lang})")
                },
            }),
            (Some(source), None) => diag.notes.push(format!("in {lang} {name} ({source})")),
            (None, _) => diag.notes.push(format!("in {lang} {name}")),
        }
    }
    if !native.is_null() {
        diag.notes
            .push(format!("carries a {} object", language_name(origin)));
    }
    if !cause.is_null() {
        if depth + 1 < MAX_CAUSE_DEPTH {
            diag.cause = Some(Box::new(report_at(cause, depth + 1)));
        } else {
            diag.notes.push("further causes omitted".to_owned());
        }
    }
    diag
}

/// A fixed palette keyed by language, wrapping.
fn lang_color(lang: LangId) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Red,
        Color::Blue,
        Color::Green,
        Color::Magenta,
        Color::Cyan,
        Color::Yellow,
        Color::BrightRed,
        Color::BrightBlue,
    ];
    PALETTE[lang as usize % PALETTE.len()]
}

type SpanId = (String, Range<usize>);

/// ariadne's cache over a `SourceLookup`: each source is fetched once per
/// render.
struct Sources<'a> {
    lookup: &'a dyn SourceLookup,
    loaded: HashMap<String, Source<String>>,
}

#[derive(Debug)]
struct UnknownSource;

impl Cache<String> for Sources<'_> {
    type Storage = String;

    fn fetch(&mut self, id: &String) -> Result<&Source<String>, impl fmt::Debug> {
        if !self.loaded.contains_key(id) {
            let Some(text) = self.lookup.source_text(id) else {
                return Err(UnknownSource);
            };
            self.loaded
                .insert(id.clone(), Source::from(text.into_owned()));
        }
        Ok(&self.loaded[id])
    }

    fn display<'a>(&self, id: &'a String) -> Option<impl fmt::Display + 'a> {
        Some(id)
    }
}

/// Draw `diag`, then each cause as a further report whose message begins
/// `caused by:`. A label whose source `sources` cannot resolve is written as
/// a note instead.
pub fn render(
    diag: &Diagnostic,
    sources: &dyn SourceLookup,
    out: &mut dyn Write,
    color: bool,
) -> io::Result<()> {
    let mut cache = Sources {
        lookup: sources,
        loaded: HashMap::new(),
    };
    let mut current = Some(diag);
    let mut first = true;
    while let Some(diag) = current {
        let message = if first {
            diag.message.clone()
        } else {
            format!("caused by: {}", diag.message)
        };
        render_one(diag, &message, &mut cache, out, color)?;
        first = false;
        current = diag.cause.as_deref();
    }
    Ok(())
}

fn render_one(
    diag: &Diagnostic,
    message: &str,
    cache: &mut Sources<'_>,
    out: &mut dyn Write,
    color: bool,
) -> io::Result<()> {
    let mut labels = Vec::new();
    let mut notes = Vec::new();
    for label in &diag.labels {
        if cache.fetch(&label.source).is_ok() {
            labels.push(label);
        } else {
            notes.push(format!(
                "in {} {} ({}:{}..{})",
                language_name(label.lang),
                label.name,
                label.source,
                label.span.0,
                label.span.1
            ));
        }
    }
    notes.extend(diag.notes.iter().cloned());

    let primary: SpanId = labels
        .first()
        .map(|l| (l.source.clone(), l.span.0..l.span.1))
        .unwrap_or_else(|| (String::new(), 0..0));
    let mut report = Report::build(ReportKind::Error, primary)
        .with_code(kind_name(diag.kind))
        .with_message(message)
        .with_config(
            Config::default()
                .with_color(color)
                .with_index_type(IndexType::Byte),
        );
    for (order, label) in labels.iter().enumerate() {
        report = report.with_label(
            Label::new((label.source.clone(), label.span.0..label.span.1))
                .with_message(&label.message)
                .with_color(lang_color(label.lang))
                .with_order(order as i32),
        );
    }
    // ariadne prints notes inside a source section; without one they are
    // written here in its format.
    if labels.is_empty() {
        report.finish().write(&mut *cache, &mut *out)?;
        let keyword = |text: String| {
            if color {
                text.fg(Color::Fixed(115)).to_string()
            } else {
                text
            }
        };
        for (i, note) in notes.iter().enumerate() {
            let prefix = if notes.len() > 1 {
                format!("Note {}", i + 1)
            } else {
                "Note".to_owned()
            };
            writeln!(out, "   {}: {note}", keyword(prefix))?;
        }
        return Ok(());
    }
    for note in notes {
        report = report.with_note(note);
    }
    report.finish().write(&mut *cache, &mut *out)
}

/// [`render`] into a string.
pub fn render_string(diag: &Diagnostic, sources: &dyn SourceLookup, color: bool) -> String {
    let mut buf = Vec::new();
    let _ = render(diag, sources, &mut buf, color);
    String::from_utf8(buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap;
    use crate::world::LANG_CORE;

    const HAXE: LangId = 3_001;
    const WREN: LangId = 3_002;

    fn sources() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            (
                "Main.hx",
                "class Main {\n  static function main() {\n    Player.hit(3);\n  }\n}\n",
            ),
            (
                "player.wren",
                "class Player {\n  static hit(n) {\n    Fiber.abort(\"no\")\n  }\n}\n",
            ),
        ])
    }

    #[test]
    fn a_framed_error_with_a_cause_renders_every_part() {
        let _lock = heap::gc_guard();
        let e = Error::new_rooted(ErrorKind::User, "player took a hit it could not take", WREN);
        let p = e.ptr() as *mut Error;
        let cause = Error::new_rooted(ErrorKind::Arithmetic, "hit points below zero", WREN);
        unsafe {
            Error::with_cause(p, cause.value());
            Error::push_frame(p, WREN, "hit", Some("player.wren"), Some((37, 54)));
            Error::push_frame(p, HAXE, "main", Some("Main.hx"), Some((45, 58)));
            Error::push_segment(p, LANG_CORE, "<callable>");
        }
        let native = Str::new_rooted("wren payload");
        unsafe { Error::set_native(p, native.value()) };

        let diag = report(e.value());
        assert_eq!(diag.kind, ErrorKind::User);
        assert_eq!(diag.labels.len(), 2);
        assert_eq!(diag.labels[0].name, "hit");
        assert_eq!(diag.labels[0].source, "player.wren");
        assert_eq!(diag.labels[0].span, (37, 54));
        assert_eq!(diag.labels[1].lang, HAXE);
        assert_eq!(
            diag.notes,
            vec![
                "in core <callable>".to_owned(),
                format!("carries a {} object", language_name(WREN)),
            ]
        );
        let cause = diag.cause.as_deref().expect("a cause");
        assert_eq!(cause.kind, ErrorKind::Arithmetic);
        assert!(cause.cause.is_none());

        let plain = render_string(&diag, &sources(), false);
        assert!(plain.contains("User"), "{plain}");
        assert!(
            plain.contains("player took a hit it could not take"),
            "{plain}"
        );
        assert!(plain.contains("hit"), "{plain}");
        assert!(plain.contains("main"), "{plain}");
        assert!(plain.contains("Fiber.abort(\"no\")"), "{plain}");
        assert!(plain.contains("Player.hit(3);"), "{plain}");
        assert!(
            plain.contains("[Arithmetic] Error: caused by: hit points below zero"),
            "{plain}"
        );
        assert!(plain.contains("carries a"), "{plain}");
        assert!(
            !plain.contains("\x1b["),
            "plain output carries no escapes: {plain}"
        );

        let coloured = render_string(&diag, &sources(), true);
        assert!(coloured.contains("\x1b["), "{coloured}");
    }

    #[test]
    fn an_error_without_spans_is_a_header_and_notes() {
        let _lock = heap::gc_guard();
        let e = Error::new_rooted(ErrorKind::Type, "an int is not callable", LANG_CORE);
        let p = e.ptr() as *mut Error;
        unsafe {
            Error::push_segment(p, LANG_CORE, "<callable>");
            Error::push_segment(p, HAXE, "run");
        }
        let diag = report(e.value());
        assert!(diag.labels.is_empty());
        assert_eq!(diag.notes.len(), 2);

        let plain = render_string(&diag, &NoSources, false);
        let lines: Vec<&str> = plain.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3, "{plain}");
        assert_eq!(lines[0], "[Type] Error: an int is not callable");
        assert_eq!(lines[1], "   Note 1: in core <callable>");
        assert_eq!(
            lines[2],
            format!("   Note 2: in {} run", language_name(HAXE))
        );
        let coloured = render_string(&diag, &NoSources, true);
        assert!(coloured.contains("\x1b["), "{coloured}");
    }

    #[test]
    fn a_label_whose_source_is_unknown_becomes_a_note() {
        let _lock = heap::gc_guard();
        let e = Error::new_rooted(ErrorKind::Index, "out of range", HAXE);
        let p = e.ptr() as *mut Error;
        unsafe { Error::push_frame(p, HAXE, "at", Some("Gone.hx"), Some((1, 4))) };
        let diag = report(e.value());
        assert_eq!(diag.labels.len(), 1);
        let plain = render_string(&diag, &NoSources, false);
        assert!(plain.contains("[Index] Error: out of range"), "{plain}");
        assert!(plain.contains("Gone.hx:1..4"), "{plain}");
    }

    #[test]
    fn a_non_error_value_reports_as_a_payload() {
        let _lock = heap::gc_guard();
        let s = Str::new_rooted("just a string");
        let diag = report(s.value());
        assert_eq!(diag.kind, ErrorKind::User);
        assert_eq!(diag.message, "just a string");
        assert_eq!(report(Value::int(4)).message, "an int");
        let plain = render_string(&diag, &NoSources, false);
        assert!(plain.contains("just a string"));
    }

    #[test]
    fn a_hand_built_diagnostic_renders_like_a_reported_one() {
        let diag = Diagnostic::new(ErrorKind::Runtime, "unknown method `hit`")
            .with_label(DiagLabel {
                lang: HAXE,
                name: "main".into(),
                source: "Main.hx".into(),
                span: (45, 58),
                message: "called here".into(),
            })
            .with_note("did you mean `hurt`?")
            .with_cause(Diagnostic::new(ErrorKind::Runtime, "no such member"));
        let plain = render_string(&diag, &sources(), false);
        assert!(
            plain.contains("[Runtime] Error: unknown method `hit`"),
            "{plain}"
        );
        assert!(plain.contains("Player.hit(3);"), "{plain}");
        assert!(plain.contains("did you mean `hurt`?"), "{plain}");
        assert!(
            plain.contains("[Runtime] Error: caused by: no such member"),
            "{plain}"
        );
    }
}
