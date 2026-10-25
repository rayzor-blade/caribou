//! What a run, a build or a serve found wrong, for a tool: one JSON object
//! a line in the file `--diagnostics <path>` names, which an IDE tails
//! beside the command's own output. The file is appended to, so a serve and
//! the builds it starts write to one file; a tool gives each job its own.
//!
//! A finding's `severity` is LSP's number (1 error, 2 warning, 3
//! information, 4 hint), its `source` what found it (`compile`, `reload`,
//! `runtime`), and its `range` 1-based lines and columns in code points,
//! the end exclusive, in the absolute `file`. A finding with no position
//! has neither. `serving` records the address a serve listens on.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::Serialize;

pub const ERROR: u8 = 1;
pub const WARNING: u8 = 2;

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: u8,
    pub source: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<Frame>,
}

impl Finding {
    pub fn new(severity: u8, source: &'static str, message: impl Into<String>) -> Finding {
        Finding {
            severity,
            source,
            message: message.into(),
            language: None,
            module: None,
            file: None,
            range: None,
            notes: Vec::new(),
            stack: Vec::new(),
        }
    }
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// A frame of a runtime error's stack, innermost first.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub function: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// The file findings go to.
pub struct Findings {
    file: Mutex<std::fs::File>,
}

impl Findings {
    pub fn open(path: &Path) -> Result<Findings> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening the diagnostics file {}", path.display()))?;
        Ok(Findings {
            file: Mutex::new(file),
        })
    }

    /// Write `finding` as one line, at once: a tool reads the file while
    /// the command runs.
    pub fn record(&self, finding: &Finding) {
        self.line(&serde_json::to_string(finding).expect("a finding serializes"));
    }

    /// The address a serve listens on.
    pub fn serving(&self, url: &str) {
        self.line(&serde_json::json!({ "serving": url }).to_string());
    }

    fn line(&self, text: &str) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(file, "{text}");
        let _ = file.flush();
    }
}

/// The findings in what `haxe` printed, run from `cwd`: `file:line:
/// characters a-b : message`, the characters 1-based bytes of the line with
/// the end exclusive, or `lines a-b`; a line that starts `... ` adds a note
/// to the finding before it, and a line with no position is a finding with
/// none. A message that starts `Warning : ` is a warning.
pub fn haxe_findings(output: &str, cwd: &Path) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let Some((file, line_no, span, message)) = haxe_line(line) else {
            out.push(Finding::new(ERROR, "compile", line.trim()).with_language("haxe"));
            continue;
        };
        if let Some(note) = message.strip_prefix("... ")
            && let Some(last) = out.last_mut()
        {
            last.notes.push(note.to_owned());
            continue;
        }
        let (severity, message) = match message.strip_prefix("Warning : ") {
            Some(rest) => (WARNING, rest),
            None => (ERROR, message),
        };
        let file = if Path::new(file).is_absolute() {
            PathBuf::from(file)
        } else {
            cwd.join(file)
        };
        let text = std::fs::read_to_string(&file).ok();
        let range = match span {
            Span::Characters(a, b) => {
                let line_text = text
                    .as_deref()
                    .and_then(|t| t.lines().nth(line_no as usize - 1))
                    .unwrap_or("");
                Range {
                    start: Position {
                        line: line_no,
                        column: code_point_column(line_text, a),
                    },
                    end: Position {
                        line: line_no,
                        column: code_point_column(line_text, b),
                    },
                }
            }
            Span::Lines(a, b) => Range {
                start: Position { line: a, column: 1 },
                end: Position {
                    line: b + 1,
                    column: 1,
                },
            },
        };
        let mut finding = Finding::new(severity, "compile", message).with_language("haxe");
        finding.file = Some(file);
        finding.range = Some(range);
        out.push(finding);
    }
    out
}

impl Finding {
    fn with_language(mut self, language: &str) -> Finding {
        self.language = Some(language.to_owned());
        self
    }
}

enum Span {
    Characters(u32, u32),
    Lines(u32, u32),
}

/// `file:line: characters a-b : message` or `file:line: lines a-b :
/// message`, split.
fn haxe_line(line: &str) -> Option<(&str, u32, Span, &str)> {
    let (head, message) = line.split_once(" : ")?;
    let (place, span) = head.rsplit_once(": ")?;
    let (file, line_no) = place.rsplit_once(':')?;
    let line_no: u32 = line_no.parse().ok()?;
    let (kind, range) = span.split_once(' ')?;
    let (a, b) = range.split_once('-')?;
    let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
    let span = match kind {
        "characters" | "character" => Span::Characters(a, b),
        "lines" | "line" => Span::Lines(a, b),
        _ => return None,
    };
    (line_no >= 1).then_some((file, line_no, span, message))
}

/// The 1-based code-point column of the 1-based byte column `byte` of
/// `line`; past the line's end it counts on by bytes.
fn code_point_column(line: &str, byte: u32) -> u32 {
    let offset = (byte.max(1) - 1) as usize;
    if offset > line.len() {
        return line.chars().count() as u32 + (offset - line.len()) as u32 + 1;
    }
    let mut end = offset;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].chars().count() as u32 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haxe_errors_become_findings_with_code_point_ranges_and_notes() {
        let dir = std::env::temp_dir().join(format!("caribou-findings-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/Main.hx"),
            "class Main {\n  static function main() {\n    var x:Int = \"héllo\";\n    trace(undefinedThing);\n  }\n}\n",
        )
        .unwrap();
        let output = "src/Main.hx:3: characters 5-25 : String should be Int\n\
                      src/Main.hx:4: characters 11-25 : Unknown identifier : undefinedThing\n\
                      src/Main.hx:4: characters 11-25 : ... For function argument 'v'\n\
                      src/Main.hx:2: lines 2-5 : Warning : This is long\n\
                      Error: Library nothing is not installed\n";
        let found = haxe_findings(output, &dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(found.len(), 4, "{found:?}");
        let first = &found[0];
        assert_eq!((first.severity, first.source), (ERROR, "compile"));
        assert_eq!(first.message, "String should be Int");
        assert_eq!(
            first.file.as_deref(),
            Some(dir.join("src/Main.hx").as_path())
        );
        // `é` is two bytes: byte 25 is code point 24.
        assert_eq!(
            first.range,
            Some(Range {
                start: Position { line: 3, column: 5 },
                end: Position {
                    line: 3,
                    column: 24
                }
            })
        );
        assert_eq!(found[1].message, "Unknown identifier : undefinedThing");
        assert_eq!(found[1].notes, ["For function argument 'v'"]);
        assert_eq!(found[2].severity, WARNING);
        assert_eq!(found[2].range.unwrap().end.line, 6);
        assert_eq!(found[3].message, "Error: Library nothing is not installed");
        assert!(found[3].file.is_none() && found[3].range.is_none());
    }

    #[test]
    fn findings_append_one_json_line_each() {
        let path =
            std::env::temp_dir().join(format!("caribou-findings-{}.jsonl", std::process::id()));
        std::fs::remove_file(&path).ok();
        let findings = Findings::open(&path).unwrap();
        findings.record(&Finding::new(ERROR, "runtime", "boom"));
        findings.serving("http://localhost:8080/");
        let again = Findings::open(&path).unwrap();
        again.record(&Finding::new(WARNING, "reload", "slow"));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["severity"], 1);
        assert_eq!(lines[0]["source"], "runtime");
        assert!(lines[0].get("file").is_none());
        assert_eq!(lines[1]["serving"], "http://localhost:8080/");
        assert_eq!(lines[2]["source"], "reload");
    }
}
