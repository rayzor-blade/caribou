//! One program in one world.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use caribou::bridge;
use caribou::error::Error;
use caribou::registry;
use caribou::report::Report;
use caribou::world::{Config, Event, EventKind, LANG_CORE, World};
use caribou_abi::{ErrorKind, Value};
use caribou_ash::{Mode, Options as AshOptions, Program};
use wren_lift::runtime::engine::ExecutionMode;
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Hybrid,
            wren_mode: ExecutionMode::Tiered,
            roots: Vec::new(),
            args: Vec::new(),
            report: false,
        }
    }
}

/// A program loaded into a world with every resident language, ready to
/// run. One per process: the runtimes' seams are process-wide.
pub struct Session {
    world: World,
    program: Program,
    vm: VM,
    report_wanted: bool,
}

impl Session {
    /// Open the program at `path`: install both seams, load it, build the
    /// world from what it imports and where the project keeps its
    /// modules, publish its classes, and make the Wren VM its modules
    /// load into.
    pub fn open(path: &Path, options: Options) -> Result<Session> {
        if !path.is_file() {
            return Err(anyhow!("{} is not a file", path.display()));
        }
        // Both seams before either runtime allocates: loading the program
        // makes the heap.
        caribou_ash::install().map_err(|e| anyhow!("ash: {e}"))?;
        caribou_wren::install().map_err(|e| anyhow!("wren_lift: {e}"))?;
        let mut program = caribou_ash::load(
            path,
            AshOptions {
                mode: options.mode,
                args: options.args,
                ..AshOptions::default()
            },
        )?;

        let roots = if options.roots.is_empty() {
            project::roots(path)
        } else {
            options.roots
        };
        let imported: Vec<String> = program
            .imports()?
            .into_iter()
            .map(|(namespace, _)| namespace)
            .collect();
        let world = World::new(Config {
            namespaces: project::namespaces(&roots, &imported),
            roots,
            ..Config::default()
        });
        world
            .register(Box::new(caribou_ash::Runtime::new()))
            .map_err(|e| anyhow!("registering haxe: {e}"))?;
        world
            .register(Box::new(caribou_wren::Runtime::new()))
            .map_err(|e| anyhow!("registering wren: {e}"))?;
        // The program's classes are what the other languages import.
        program.publish()?;
        // A module's file edited while the program runs reloads it.
        world.watch_sources();

        let mut config = VMConfig {
            execution_mode: options.wren_mode,
            gc_strategy: GcStrategy::Immix,
            ..VMConfig::default()
        };
        caribou_wren::import::configure(&mut config);
        let mut vm = VM::new(config);
        // Every Wren fiber on a stack of its own: the core scans them as
        // it scans its own tasks' (see `caribou_wren::install`).
        vm.krio_fiber_active = true;
        if options.report {
            caribou_wren::report::count_entries(&mut vm);
        }
        Ok(Session {
            world,
            program,
            vm,
            report_wanted: options.report,
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
        self.world.on(EventKind::Reload, |event| {
            let Event::Reload {
                lang,
                module,
                error,
            } = event;
            let lang = caribou::world::language_name(*lang);
            match error {
                None => eprintln!("[caribou] reloaded {lang} {module}"),
                Some(error) => eprintln!("[caribou] {lang} {module} did not reload: {error}"),
            }
        });
        let result = self.start();
        if self.report_wanted {
            eprint!("{}", self.report());
        }
        self.program.finish();
        result
    }
}

/// Open and run the program at `path`.
pub fn run(path: &Path, options: Options) -> Result<()> {
    Session::open(path, options)?.run()
}
