//! One program in one world.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use caribou::bridge;
use caribou::bundle;
use caribou::error::Error;
use caribou::registry;
use caribou::report::Report;
use caribou::world::{Config, Event, EventKind, LANG_CORE, World};
use caribou_abi::{ErrorKind, Value};
use caribou_ash::{Mode, Options as AshOptions, Program, TierPreset};
use wren_lift::runtime::engine::ExecutionMode;
use wren_lift::runtime::vm::{VM, VMConfig};

use crate::cbproj::{HaxeFailed, Project};
use crate::findings::{self, Finding, Findings};
use crate::project;

/// What a session is opened with.
pub struct Options {
    /// Ash's execution mode for the program.
    pub mode: Mode,
    /// wren_lift's execution mode for the Wren modules.
    pub wren_mode: ExecutionMode,
    /// The source roots, when not the project's own (`project::roots`).
    pub roots: Vec<PathBuf>,
    /// The program's arguments.
    pub args: Vec<String>,
    /// Count what the run does, for [`Session::report`]: how often each
    /// Wren function is entered, per tier.
    pub report: bool,
    /// Let the Haxe program reload in place when its file changes, as the
    /// other languages' modules do. Ash then lowers what its interpreter
    /// walks without inlining and enters no interpreted loop by OSR, so a
    /// measurement of the program itself turns this off.
    pub reload: bool,
    /// Ash's tier thresholds in hybrid mode (`--preset`).
    pub preset: TierPreset,
    /// Where to record what the run finds wrong, as JSON lines
    /// (`--diagnostics`; see [`crate::findings`]).
    pub diagnostics: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Hybrid,
            wren_mode: ExecutionMode::Tiered,
            roots: Vec::new(),
            args: Vec::new(),
            report: false,
            reload: true,
            preset: TierPreset::Application,
            diagnostics: None,
        }
    }
}

/// A program loaded into a world with every resident language, ready to
/// run, from its file and the project around it or from a bundle. One
/// per process: the runtimes' seams are process-wide.
pub struct Session {
    world: World,
    program: Program,
    vm: VM,
    report_wanted: bool,
    findings: Option<std::sync::Arc<Findings>>,
    /// Where the program's own source files are named from: the project's
    /// directory, then its roots.
    source_dirs: Vec<PathBuf>,
}

impl Session {
    /// Open the program at `path`, a `.hl` or a bundle: install both
    /// seams, load it, build the world from what it imports and where
    /// the project keeps its modules, or from the bundle's manifest and
    /// modules, publish its classes, and make the Wren VM its modules
    /// load into.
    pub fn open(path: &Path, options: Options) -> Result<Session> {
        let findings = findings_for(&options)?;
        let base = std::env::current_dir().unwrap_or_default();
        Self::opened(Self::open_program(path, options), findings, vec![base])
    }

    /// Open the project a project file declares: its Haxe entry compiled,
    /// and its roots, languages, plugins and packages as declared rather
    /// than found.
    pub fn open_project(project: &Project, options: Options) -> Result<Session> {
        let findings = findings_for(&options)?;
        let opened = Self::open_declared(project, options, findings.as_deref());
        let mut dirs = vec![project.dir.clone()];
        dirs.extend(project.sources.iter().cloned());
        Self::opened(opened, findings, dirs)
    }

    /// A session opened, given what it records findings in; a failure to
    /// open is a finding too, unless the compiler's own are recorded.
    fn opened(
        opened: Result<Session>,
        findings: Option<std::sync::Arc<Findings>>,
        source_dirs: Vec<PathBuf>,
    ) -> Result<Session> {
        match opened {
            Ok(mut session) => {
                session.findings = findings;
                session.source_dirs = source_dirs;
                Ok(session)
            }
            Err(e) => {
                if let Some(findings) = &findings
                    && e.downcast_ref::<HaxeFailed>().is_none()
                {
                    findings.record(&error_finding("compile", &e));
                }
                Err(e)
            }
        }
    }

    fn open_program(path: &Path, options: Options) -> Result<Session> {
        if !path.is_file() {
            return Err(anyhow!("{} is not a file", path.display()));
        }
        // Both seams before either runtime allocates: loading the program
        // makes the heap. Ash's goes into the image the program runs
        // through, which its HDLLs decide.
        caribou_ash::install_for(path).map_err(|e| anyhow!("ash: {e}"))?;
        caribou_wren::install().map_err(|e| anyhow!("wren_lift: {e}"))?;
        let ash_options = AshOptions {
            mode: options.mode,
            args: options.args,
            reload: options.reload,
            preset: options.preset.clone(),
            ..AshOptions::default()
        };
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bundle::looks_like(&bytes) {
            let bundle = bundle::load(&bytes).with_context(|| path.display().to_string())?;
            // A bundle's plugins are its own native library sections,
            // and its languages come up from its language sections.
            let plugin_dir = crate::bundle::native_libs(&bundle)?;
            let plugins = caribou_plugin::load_dir(&plugin_dir).map_err(|e| anyhow!("{e}"))?;
            let frontends = crate::bundle::frontends(&bundle, &plugin_dir)?;
            let entry = bundle
                .entry()
                .ok_or_else(|| anyhow!("{} carries no entry module", path.display()))?;
            if entry.lang != "haxe" || entry.format != "hl" {
                return Err(anyhow!(
                    "{} starts with {} {} `{}`; only a haxe hl program starts a session",
                    path.display(),
                    entry.lang,
                    entry.format,
                    entry.name
                ));
            }
            let program = caribou_ash::load_bytes(&entry.data, path, ash_options)?;
            let config = Config {
                name: bundle.manifest.name.clone(),
                namespaces: bundle.manifest.namespaces.clone(),
                roots: Vec::new(),
            };
            return Self::finish(
                program,
                config,
                plugins,
                frontends,
                Some(&bundle),
                options.wren_mode,
                options.report,
            );
        }
        drop(bytes);
        // The plugins beside the program, in `plugins/`: what the world
        // grants every language.
        let plugin_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins");
        let plugins = caribou_plugin::load_dir(&plugin_dir).map_err(|e| anyhow!("{e}"))?;
        let program = caribou_ash::load(path, ash_options)?;
        let roots = if options.roots.is_empty() {
            project::roots(path)
        } else {
            options.roots
        };
        // The hatch packages the roots' hatchfiles depend on, for Wren.
        for package in caribou_wren::hatch::dependencies(&roots).map_err(|e| anyhow!(e))? {
            caribou_wren::hatch::hold(package);
        }
        // The Zyntax frontends under the roots, each a language; their
        // `.zrtl` plugins sit in `plugins/` beside the program too.
        let frontends = project::frontends(&roots, &plugin_dir)?;
        Self::open_with(
            program,
            roots,
            plugins,
            frontends,
            options.wren_mode,
            options.report,
        )
    }

    fn open_declared(
        project: &Project,
        options: Options,
        findings: Option<&Findings>,
    ) -> Result<Session> {
        let hl = project.compile_haxe_into(findings)?;
        caribou_ash::install_for(&hl).map_err(|e| anyhow!("ash: {e}"))?;
        caribou_wren::install().map_err(|e| anyhow!("wren_lift: {e}"))?;
        let ash_options = AshOptions {
            mode: options.mode,
            args: options.args,
            reload: options.reload,
            preset: options.preset.clone(),
            ..AshOptions::default()
        };
        let plugins = project.load_plugins()?;
        let program = caribou_ash::load(&hl, ash_options)?;
        for package in crate::deps::hatch(project)? {
            caribou_wren::hatch::hold(package);
        }
        let frontends = project.frontends()?;
        for dependency in crate::deps::unresolved(project)? {
            eprintln!(
                "[caribou] {dependency}: not installed; Zyntax has no package support for its language yet"
            );
        }
        let roots = project.sources.clone();
        Self::open_with(
            program,
            roots,
            plugins,
            frontends,
            options.wren_mode,
            options.report,
        )
    }

    /// The world around a loaded `.hl` program, from where its modules,
    /// languages and plugins are: the namespaces it imports and its roots
    /// hold, over every language.
    fn open_with(
        program: Program,
        roots: Vec<PathBuf>,
        plugins: Vec<caribou_plugin::Plugin>,
        frontends: Vec<caribou_zyntax::Frontend>,
        wren_mode: ExecutionMode,
        report: bool,
    ) -> Result<Session> {
        let imported: Vec<String> = program
            .imports()?
            .into_iter()
            .map(|(namespace, _)| namespace)
            .collect();
        // A frontend's modules are files under the project's directories;
        // a plugin's classes are reached through its own namespace only.
        let others: Vec<String> = frontends.iter().map(|f| f.name().to_owned()).collect();
        let plugin_names: Vec<String> = plugins.iter().map(|p| p.name().to_owned()).collect();
        let config = Config {
            namespaces: project::namespaces(&roots, &imported, &others, &plugin_names),
            roots,
            ..Config::default()
        };
        Self::finish(program, config, plugins, frontends, None, wren_mode, report)
    }

    /// The world around a loaded program: the adapters, the plugins as
    /// languages of their own, the bundle's modules when there is one,
    /// else a watch on the sources, the program's classes published, and
    /// the Wren VM.
    fn finish(
        mut program: Program,
        config: Config,
        plugins: Vec<caribou_plugin::Plugin>,
        frontends: Vec<caribou_zyntax::Frontend>,
        bundle: Option<&bundle::Bundle>,
        wren_mode: ExecutionMode,
        report: bool,
    ) -> Result<Session> {
        let world = World::new(config);
        world
            .register(Box::new(caribou_ash::Runtime::new()))
            .map_err(|e| anyhow!("registering haxe: {e}"))?;
        world
            .register(Box::new(caribou_wren::Runtime::new()))
            .map_err(|e| anyhow!("registering wren: {e}"))?;
        if !plugins.is_empty() {
            world
                .register(Box::new(caribou_plugin::Runtime::new(plugins)))
                .map_err(|e| anyhow!("registering the plugins: {e}"))?;
        }
        if !frontends.is_empty() {
            world
                .register(Box::new(caribou_zyntax::Runtime::new(frontends)))
                .map_err(|e| anyhow!("registering the zyntax frontends: {e}"))?;
        }
        match bundle {
            Some(bundle) => world.install(bundle).map_err(|e| anyhow!(e))?,
            // A module's file edited while the program runs reloads it.
            None => world.watch_sources(),
        }
        // The program's classes are what the other languages import.
        program.publish()?;

        let mut config = VMConfig {
            execution_mode: wren_mode,
            ..VMConfig::default()
        };
        caribou_wren::import::configure(&mut config);
        let mut vm = VM::new(config);
        // Every Wren fiber on a stack of its own: the core scans them as
        // it scans its own tasks' (see `caribou_wren::install`).
        vm.krio_fiber_active = true;
        caribou_wren::hatch::stage(&mut vm).map_err(|e| anyhow!(e))?;
        if report {
            caribou_wren::report::count_entries(&mut vm);
        }
        Ok(Session {
            world,
            program,
            vm,
            report_wanted: report,
            findings: None,
            source_dirs: Vec::new(),
        })
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The Wren VM the session's modules load into.
    pub fn wren(&mut self) -> &mut VM {
        &mut self.vm
    }

    /// Run the program's entry point and its event loop, and keep the
    /// session: what it published stays callable through [`Self::call`].
    pub fn start(&mut self) -> Result<()> {
        caribou_wren::with_vm(&mut self.vm, |_| self.program.start())
    }

    /// Call the static member `member` of the class `class` of the module
    /// `namespace:module`, whichever language it is, loading the module on
    /// first use. `Err` is the error value the bridge answers with.
    pub fn call(
        &mut self,
        namespace: &str,
        module: &str,
        class: &str,
        member: &str,
        args: &[Value],
    ) -> Result<Value, Value> {
        let missing =
            |message: String| Error::value(Error::new(ErrorKind::Runtime, &message, LANG_CORE));
        // A Haxe reload staged since the program last ran applies now, as
        // it would when the program's own call returned.
        self.program.poll_reload();
        // Entered for the lookup too: a module loading on first use needs
        // the VM.
        caribou_wren::with_vm(&mut self.vm, |_| {
            let (iface, index) = registry::lookup_class_or_load(namespace, module, class)
                .map_err(missing)?
                .ok_or_else(|| missing(format!("{namespace}:{module} has no class {class}")))?;
            let class = &iface.classes[index];
            let target = class
                .methods
                .iter()
                .find(|m| m.is_static && m.name == member)
                .map(|m| m.target)
                .ok_or_else(|| missing(format!("{} has no static {member}", class.name)))?;
            bridge::call_named(target, args, LANG_CORE, member)
        })
    }

    /// Load the module `namespace:module` afresh from the project's
    /// sources, whichever language it is (see `World::reload`): its
    /// classes keep their identity, calls from the other language reach
    /// the new bodies, and the world's `Reload` subscribers hear of it.
    pub fn reload(&mut self, namespace: &str, module: &str) -> Result<()> {
        let world = &self.world;
        caribou_wren::with_vm(&mut self.vm, |_| world.reload(namespace, module))
            .map_err(|e| anyhow!(e))
    }

    /// What the run did so far, in the program's terms: the tier each
    /// function reached, how each send across the bridge went, and what
    /// crossed boxed. Entry counts need `Options::report`.
    pub fn report(&self) -> Report {
        let mut report = Report::default();
        report.functions("Wren", caribou_wren::report::functions(&self.vm));
        report.functions(
            "Haxe, compiled",
            caribou_ash::report::compiled(&self.program),
        );
        report.sites("Haxe → Wren", caribou_ash::report::sites());
        report.sites("Wren → Haxe", caribou_wren::report::sites(&self.vm));
        report.callbacks = caribou_ash::report::callbacks();
        report
    }

    /// Run the program to the end of its entry point and its event loop,
    /// saying on stderr what reloads meanwhile. With `Options::report`,
    /// print the report after.
    pub fn run(mut self) -> Result<()> {
        let findings = self.findings.clone();
        self.world.on(EventKind::Reload, move |event| {
            let Event::Reload {
                lang,
                module,
                error,
            } = event
            else {
                return;
            };
            let name = caribou::world::language_name(*lang);
            match error {
                None => eprintln!("[caribou] reloaded {name} {module}"),
                Some(error) => {
                    eprintln!("[caribou] {name} {module} did not reload: {error}");
                    if let Some(findings) = &findings {
                        findings.record(&reload_finding(*lang, module, error));
                    }
                }
            }
        });
        let result = self.start();
        if let (Err(e), Some(findings)) = (&result, &self.findings) {
            findings.record(&runtime_finding(e, &self.source_dirs));
        }
        if self.report_wanted {
            eprint!("{}", self.report());
        }
        self.program.finish();
        result
    }
}

/// The findings file `options` names, opened.
fn findings_for(options: &Options) -> Result<Option<std::sync::Arc<Findings>>> {
    options
        .diagnostics
        .as_deref()
        .map(|path| Findings::open(path).map(std::sync::Arc::new))
        .transpose()
}

/// An error as one finding: its first line the message, the rest notes.
fn error_finding(source: &'static str, error: &anyhow::Error) -> Finding {
    let text = format!("{error:#}");
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let mut finding = Finding::new(findings::ERROR, source, lines.next().unwrap_or("").trim());
    finding.notes = lines.map(|l| l.trim().to_owned()).collect();
    finding
}

/// A reload that failed: the module's file, and the position its error's
/// rendering names (`[ path:line:column ]`) when it names one.
fn reload_finding(lang: caribou_abi::LangId, module: &str, error: &str) -> Finding {
    let mut lines = error.lines().filter(|l| !l.trim().is_empty());
    let mut finding = Finding::new(findings::ERROR, "reload", lines.next().unwrap_or("").trim());
    finding.language = Some(caribou::world::language_name(lang).to_owned());
    finding.module = Some(module.to_owned());
    finding.file = registry::sources()
        .into_iter()
        .find(|(l, m, _)| *l == lang && m == module)
        .and_then(|(_, _, path)| std::path::absolute(path).ok());
    if let Some((file, line, column)) = rendered_position(error) {
        if finding.file.is_none() {
            finding.file = std::path::absolute(file).ok();
        }
        finding.range = Some(findings::Range {
            start: findings::Position { line, column },
            end: findings::Position {
                line,
                column: column + 1,
            },
        });
    }
    finding
}

/// The `path:line:column` a rendered error's header names, `─[ path:3:7 ]`.
fn rendered_position(text: &str) -> Option<(PathBuf, u32, u32)> {
    let start = text.find("[ ")? + 2;
    let end = start + text[start..].find(" ]")?;
    let (rest, column) = text[start..end].rsplit_once(':')?;
    let (path, line) = rest.rsplit_once(':')?;
    Some((
        PathBuf::from(path),
        line.parse().ok()?,
        column.parse().ok()?,
    ))
}

/// The error a run ended with: an exception the Haxe program did not catch
/// with its stack, the innermost frame with a file its position; anything
/// else as text.
fn runtime_finding(error: &anyhow::Error, dirs: &[PathBuf]) -> Finding {
    let Some(uncaught) = caribou_ash::uncaught(error) else {
        return error_finding("runtime", error);
    };
    let mut finding = Finding::new(findings::ERROR, "runtime", uncaught.message);
    finding.language = Some("haxe".to_owned());
    finding.stack = uncaught
        .stack
        .iter()
        .map(|frame| findings::Frame {
            function: frame.function.clone(),
            file: frame.file.as_deref().map(|f| source_file(f, dirs)),
            line: u32::try_from(frame.line).ok().filter(|l| *l > 0),
        })
        .collect();
    if let Some(frame) = finding
        .stack
        .iter()
        .find(|f| f.file.is_some() && f.line.is_some())
    {
        let line = frame.line.unwrap_or(1);
        finding.file = frame.file.clone();
        finding.range = Some(findings::Range {
            start: findings::Position { line, column: 1 },
            end: findings::Position {
                line: line + 1,
                column: 1,
            },
        });
    }
    finding
}

/// The absolute path of a source file the program names as it was
/// compiled: as it is when absolute, else the first of `dirs` that holds it.
fn source_file(name: &str, dirs: &[PathBuf]) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    dirs.iter()
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| dirs.first().map(|d| d.join(path)).unwrap_or(path.into()))
}

/// Open and run the program at `path`.
pub fn run(path: &Path, options: Options) -> Result<()> {
    Session::open(path, options)?.run()
}

/// Open and run the project `project` declares, in one call.
pub fn run_project(project: &Project, options: Options) -> Result<()> {
    Session::open_project(project, options)?.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendered_error_names_its_position_in_its_header() {
        let text = "Error: expected '}'\n   ╭─[ ./src/game/hello.lua:10:26 ]\n   │\n";
        assert_eq!(
            rendered_position(text),
            Some((PathBuf::from("./src/game/hello.lua"), 10, 26))
        );
        assert_eq!(rendered_position("no position here"), None);
    }
}
