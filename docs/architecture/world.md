# World and driver

A world is what a driver holds. Through it the driver registers the
runtimes it uses, loads code, calls into it, ticks the scheduler and
subscribes to events. One world is one OS thread, one scheduler, and one
view of the process-wide heap.

`caribou::world` is the registry of adapters and their languages. It does
not own the heap or the scheduler, which are per process and per thread.
But it is the only path a driver uses to reach them.

## Adapters and languages

An adapter is a runtime that has been taught the core. It implements
`Adapter` and registers with `World::register`. Registration hands the
adapter the `LangId`s it owns: one for a single-language runtime, one per
grammar snapshot for Zyntax. A `LangId` names a namespace in the module
registry (`lang:path`), the `lang` field of every `TypeDesc` the adapter
creates, and the language a module section in a bundle belongs to.

`Adapter` supplies:

- its language names;
- `load(source: ModuleSource) -> ModuleId`, taking bytecode, source text
  or a blob in the adapter's own format;
- `lookup(module, name) -> Option<Callable>`;
- `call(callable, args: &[Value]) -> Result<Value, Error>`, through the
  bridge;
- `reload(module)`, returning a plan for the registry to apply;
- the per-task `HostState` it wants attached when the world spawns a task
  on its behalf.

## Startup and the frame loop

`World::new(config)` initialises the heap if needed, initialises the
calling thread's scheduler, and installs the heap's poll hook. The driver
then registers adapters and loads modules. After that it either calls
`World::run_main(module)`, letting the world own the loop, or calls
`World::tick(deadline)` from its own frame loop. `tick` runs scheduler
turns, drains reload checks and delivers events until the deadline. Ash's
frame pump is the first driver: the world ticks inside it, between the
Haxe application's frames.

## The driver

`caribou-driver` is what runs a program, for an embedder and for the
`caribou` command alike. `Session::open(program)` then `Session::run`, or
`run` for both. A session is one world, with every resident language,
around one Haxe program.

Opening a session installs both seams, loads the program, and reads its
`caribou` natives for the namespaces it imports (`Program::imports`). The
project's layout is the configuration. The source roots are the class
paths of the `.hxml` files in the working directory and beside the
program; else `src` under them, when it exists; else the directories
themselves (`project::roots`). Every directory under a root is a
namespace, and so is every namespace the program imports. Each covers
every resident language, Haxe first (`project::namespaces`). The world is
made from those. The two adapters register, the program's classes are
published, and a Wren VM is made with the import callbacks installed.
`run` enters the VM and starts the program. `call` reaches a published
member from outside, and `wren` hands an embedder the VM.

Nothing else is loaded up front. A module of another language loads on
first use. The registry's `resolve_or_load` asks the namespace's
languages' loaders in turn when nothing has published a module. The Wren
adapter's loader (`caribou_wren::project`) finds `game/hud.wren` under the
first root that has it, loads it into the entered VM under the path
spelling `game/hud`, which the registry resolves for `game:hud`, and
publishes it. A Haxe call through a bound native reaches that path
through `lookup_class_or_load`. A Wren `import "game:hud"` reaches it
through the resolve callback, which answers the module's own name for a
Wren module and installs the class for another language's.

Loading runs the module's top level. So it happens once the program is
running and has published, which is why the program publishes before it
starts: the interpreter registers its closure runner as it starts, before
its entry function, so a call from a Wren module's top level lands in a
running program. A Wren module's plain imports are served from the same
roots, beside the importer first.

## Events

`World::on(kind, handler)` subscribes a handler to `Reload`, `TaskError`
and `Log` events. Handlers run on the world's thread, from `tick`, never
from inside a collection or a switch.

## Boundaries of the current implementation

Built so far: the adapter registry, the language table, the namespace
table, the source roots, the loaders, and the driver above. Reload, call
and events through the world arrive with the reload pipeline. The world
does not tick yet; a session runs the program's own loop.
