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
turns, drains reload checks and delivers events until the deadline.

The Haxe application's own loop is the first driver, and it needs no
call of its own: every wait it makes reaches the scheduler through the
seam. `Sys.sleep` and a `Lock` wait park on a scheduler timer, and so
does the idle time of a frame loop installed through `sys_set_loop`,
the way a UI library installs one, while any task is alive. Between two
Haxe frames the scheduler drives what is ready and idles on its
endpoint, timers and, once it exists, the reactor.

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

## The run report

`Session::report` says what the run did, in the program's own terms
(`caribou::report`): each Wren function of the project's modules with the
tier it reached and, when the session was opened to count, how often it
was entered; each Haxe method a tier compiled; each site the program
sends across the bridge from, whether it holds a direct send and how many
sends took the plain path, and whether the member links at AOT or stays
dynamic (see [linking](linking.md)); how many scalars crossed boxed
through a `Dynamic` parameter or result; and how many closures crossed
typed or boxed. Each adapter answers for its language (`caribou_ash::report`,
`caribou_wren::report`), and the session gathers the answers. `caribou
run --report` prints it when the program ends.

## Reload

`World::reload(namespace, module)` loads a module afresh from the
project's sources, whichever language it is. The adapter that serves the
language re-runs the module in place (`Adapter::reload`): its classes keep
their identity and get the new bodies, and its interface is published
again. Then the protocol's epoch is bumped, so every call site in every
language fills again (see [bridge.md](bridge.md#call-sites)), and
subscribers hear `Event::Reload`. An object of the module made before the
reload keeps its class and so runs the new bodies, and a call the other
language bound to the class before reaches them through its site.

wren_lift reloads a module by re-running it with the class objects it
declared kept (`VM::reload_module`), its compiled bodies dropped and its
inline caches cleared; the adapter then publishes the module again and
runs the program's `Hatch.onReload` callbacks. `Session::reload` does the
same with the VM entered. Ash does not reload yet: `Adapter::reload` is an
error for Haxe.

What triggers a reload is either the driver's call or the world's own
watch on the sources. `World::watch_sources` looks at the file every
loaded module came from (`registry::sources`, which a language's loader
records) on a short interval, from a thread of its own, and when one
changes it raises a reactor source (see
[scheduler.md](scheduler.md#the-reactor)) whose handler reloads the
module on the world's thread, between scheduler turns: wherever the
program is idle, a `Sys.sleep`, a `Lock` wait, a frame's pacing. A
module loaded later is watched from then on. A source that no longer
compiles reloads nothing: the module stays as it was, and the event
carries the error. A session watches from the moment it opens, and
`caribou run` says on stderr what reloaded.

## Events

`World::on(kind, handler)` subscribes a handler to events of a kind;
`World::raise` raises one. Handlers run on the world's thread, from
`tick` and at the end of a reload, never from inside a collection or a
switch, and with the world unborrowed, so a handler may subscribe,
raise or reload. `Reload` is the one kind so far, with the error when
the load failed; a task's uncaught error and a language's log line come
as events with the paths that raise them.

## Boundaries of the current implementation

Built so far: the adapter registry, the language table, the namespace
table, the source roots, the loaders, the driver above, the reload of a
Wren module, the source watch that triggers it and the events. Ash's
reload is not built. Under a session the program's own loop drives the
scheduler, and the world's handlers run from there through the reactor;
a driver with a loop of its own hears them from `tick` as well.
