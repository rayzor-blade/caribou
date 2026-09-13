//! One program in one world.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use caribou::bridge;
use caribou::error::Error;
use caribou::registry;
use caribou::world::{Config, LANG_CORE, World};
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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Hybrid,
            wren_mode: ExecutionMode::Tiered,
            roots: Vec::new(),
            args: Vec::new(),
        }
    }
}

/// A program loaded into a world with every resident language, ready to
/// run. One per process: the runtimes' seams are process-wide.
pub struct Session {
    world: World,
    program: Program,
    vm: VM,
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
        let mut world = World::new(Config {
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

        let mut config = VMConfig {
            execution_mode: options.wren_mode,
            gc_strategy: GcStrategy::Immix,
            ..VMConfig::default()
        };
        caribou_wren::import::configure(&mut config);
        let vm = VM::new(config);
        Ok(Session { world, program, vm })
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

    /// Run the program to the end of its entry point and its event loop.
    pub fn run(mut self) -> Result<()> {
        let result = self.start();
        self.program.finish();
        result
    }
}

/// Open and run the program at `path`.
pub fn run(path: &Path, options: Options) -> Result<()> {
    Session::open(path, options)?.run()
}
