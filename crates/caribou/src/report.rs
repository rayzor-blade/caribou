//! What a run did, in the program's own terms: the tier each function
//! reached, how each send across the bridge went, and what crossed
//! untyped. Each adapter answers for its language; a driver gathers the
//! answers into a [`Report`] and prints it.

use std::fmt;

/// How a function ran by the end of the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Interpreted,
    /// The first compiled tier.
    Baseline,
    /// The optimizing tier.
    Optimized,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Interpreted => "interpreted",
            Tier::Baseline => "baseline",
            Tier::Optimized => "optimized",
        }
    }
}

/// One function of the program: its name as the language writes it, the
/// tier it reached, and how many times it was entered when the runtime
/// counted.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub tier: Tier,
    pub entries: Option<u64>,
}

/// One place a program sends across the bridge: a bound native in Haxe,
/// a host method in Wren. `direct` says whether the site holds a direct
/// send now, `plain` how many sends took the plain path, and `boxed_in`
/// and `boxed_out` how many scalars arrived and left boxed, through a
/// parameter or result the program declared `Dynamic`: a box is an
/// allocation per call, whatever the tier.
#[derive(Clone, Debug)]
pub struct Site {
    pub name: String,
    pub direct: bool,
    pub plain: usize,
    pub boxed_in: usize,
    pub boxed_out: usize,
}

/// The functions that crossed as closures: typed ones the other language
/// calls as its own, and boxed ones it calls through a var-args shim.
#[derive(Clone, Copy, Debug, Default)]
pub struct Callbacks {
    pub typed: u64,
    pub boxed: u64,
}

/// The whole run, one section per language and per direction.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub sections: Vec<Section>,
    pub callbacks: Callbacks,
}

/// A titled list of rows.
#[derive(Clone, Debug)]
pub struct Section {
    pub title: String,
    pub rows: Rows,
}

#[derive(Clone, Debug)]
pub enum Rows {
    Functions(Vec<Function>),
    Sites(Vec<Site>),
}

impl Report {
    pub fn functions(&mut self, title: &str, mut rows: Vec<Function>) {
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        self.sections.push(Section {
            title: title.to_owned(),
            rows: Rows::Functions(rows),
        });
    }

    pub fn sites(&mut self, title: &str, mut rows: Vec<Site>) {
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        self.sections.push(Section {
            title: title.to_owned(),
            rows: Rows::Sites(rows),
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .sections
            .iter()
            .flat_map(|s| match &s.rows {
                Rows::Functions(rows) => rows.iter().map(|r| r.name.len()).collect::<Vec<_>>(),
                Rows::Sites(rows) => rows.iter().map(|r| r.name.len()).collect(),
            })
            .max()
            .unwrap_or(0)
            .max(24);
        let mut boxed: Vec<&Site> = Vec::new();
        for section in &self.sections {
            writeln!(f, "{}", section.title)?;
            match &section.rows {
                Rows::Functions(rows) => {
                    if rows.is_empty() {
                        writeln!(f, "  (none)")?;
                    }
                    for r in rows {
                        write!(f, "  {:<width$}  {:<11}", r.name, r.tier.label())?;
                        if let Some(n) = r.entries {
                            write!(f, "  {n}")?;
                        }
                        writeln!(f)?;
                    }
                }
                Rows::Sites(rows) => {
                    if rows.is_empty() {
                        writeln!(f, "  (none)")?;
                    }
                    for r in rows {
                        let send = if r.direct { "direct" } else { "plain" };
                        write!(f, "  {:<width$}  {:<11}", r.name, send)?;
                        if r.plain > 0 {
                            write!(f, "  {} plain", r.plain)?;
                        }
                        writeln!(f)?;
                        if r.boxed_in + r.boxed_out > 0 {
                            boxed.push(r);
                        }
                    }
                }
            }
            writeln!(f)?;
        }
        if !boxed.is_empty() {
            writeln!(
                f,
                "Boxed: scalars through a Dynamic parameter or result, an allocation each; a type in the member's #export keeps them out of the box"
            )?;
            for r in boxed {
                write!(f, "  {:<width$}", r.name)?;
                if r.boxed_in > 0 {
                    write!(f, "  {} in", r.boxed_in)?;
                }
                if r.boxed_out > 0 {
                    write!(f, "  {} out", r.boxed_out)?;
                }
                writeln!(f)?;
            }
            writeln!(f)?;
        }
        let c = self.callbacks;
        if c.typed + c.boxed > 0 {
            writeln!(f, "Callbacks: {} typed, {} boxed", c.typed, c.boxed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_sections_in_order_and_gathers_what_crossed_boxed() {
        let mut r = Report::default();
        r.functions(
            "Wren",
            vec![Function {
                name: "Boid.update(_)".into(),
                tier: Tier::Baseline,
                entries: Some(12),
            }],
        );
        r.sites(
            "Wren → Haxe",
            vec![
                Site {
                    name: "World.x(_)".into(),
                    direct: true,
                    plain: 1,
                    boxed_in: 0,
                    boxed_out: 0,
                },
                Site {
                    name: "Tally.new(_)".into(),
                    direct: false,
                    plain: 3,
                    boxed_in: 3,
                    boxed_out: 0,
                },
            ],
        );
        r.callbacks = Callbacks { typed: 2, boxed: 0 };
        let text = r.to_string();
        assert!(text.starts_with("Wren\n  Boid.update(_)"));
        assert!(text.contains("baseline     12\n"));
        assert!(text.contains("World.x(_)"));
        assert!(text.contains("direct       1 plain"));
        let boxed = text.split("Boxed:").nth(1).expect("a boxed section");
        assert!(boxed.contains("Tally.new(_)") && boxed.contains("  3 in\n"));
        assert!(!boxed.contains("World.x(_)"));
        assert!(text.ends_with("Callbacks: 2 typed, 0 boxed\n"));
    }
}
