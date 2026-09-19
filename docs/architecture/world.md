# World & Driver

## Overview

A world is the object a driver holds. Through it, the driver registers the runtimes it uses, loads code, calls into that code, ticks the scheduler, and subscribes to events. One world corresponds to one OS thread, one scheduler, and one view of the process-wide heap.

`caribou::world` is the registry of adapters and their languages. It does not own the heap or the scheduler, which are per process and per thread, but it is the only path a driver uses to reach them.

## Adapters & Languages

An adapter is a runtime that has been integrated with the core. It implements `Adapter` and registers with `World::register`. Registration gives the adapter the `LangId`s it owns: one for a single-language runtime, and one per snapshot for Zyntax. A `LangId` names a namespace in the module registry (`lang:path`), fills the `lang` field of every `TypeDesc` the adapter creates, and identifies the language a module section in a bundle belongs to.

An `Adapter` provides:

* Its language names, and it accepts the ids the world assigns to them.
* `reload(lang, module)`, which loads a module again in place (see below).
* `install(lang, section)`, which accepts a module of its language from a bundle (see [bundle.md](bundle.md)).

Lookup and calls go through the registry and the bridge, not through the adapter. What a language publishes is what the other languages can reach. The host state an adapter keeps per task and per stack is attached through the scheduler's hooks (see [scheduler.md](scheduler.md)).

## Startup & Frame Loop

`World::new(config)` initializes the heap if needed, initializes the calling thread's scheduler, and installs the heap's poll hook. The driver then registers adapters and loads modules. After that, the driver either calls `World::run_main(module)` and lets the world own the loop, or calls `World::tick(deadline)` from its own frame loop. `tick` runs scheduler turns, drains pending reload checks, and delivers events until the deadline.

**Haxe as the driver:** The Haxe application's own loop is the first driver, and it does not need to call the world at all. Every wait it performs reaches the scheduler through the seam. `Sys.sleep` and a `Lock` wait park on a scheduler timer. So does the idle time of a frame loop installed through `sys_set_loop`, the way a UI library installs one, for as long as any task is alive. Between two Haxe frames, the scheduler runs whatever is ready and then idles on its endpoint, its timers, and the reactor.

## The Driver

`caribou-driver` runs a program. It serves both embedders and the `caribou` command. The API is `Session::open(program)` followed by `Session::run`, or `run` to do both. A session is one world, with every resident language, around one Haxe program.

**Opening a session:**

1. The session installs both seams, loads the program, and reads the program's `caribou` natives to find the namespaces it imports (`Program::imports`). A bundle carries the namespaces and the modules instead (see [bundle.md](bundle.md)).
2. The project's layout is the configuration. The source roots are the class paths of the `.hxml` files in the working directory and next to the program; otherwise `src` under those directories, if it exists; otherwise the directories themselves (`project::roots`).
3. Every directory under a root becomes a namespace, and so does every namespace the program imports. Each namespace covers every resident language, with Haxe first (`project::namespaces`). The world is created from these namespaces.
4. The adapters register, the program's classes are published, and a Wren VM is created with the import callbacks installed.

`run` enters the VM and starts the program. `call` reaches a published member from outside the program, and `wren` gives an embedder access to the VM.

**Loading on first use:** Nothing else is loaded up front. A module of another language loads the first time it is used. The registry's `resolve_or_load` asks the namespace's language loaders in turn when no module has been published. The Wren adapter's loader (`caribou_wren::project`) finds `game/hud.wren` under the first root that has it, loads it into the entered VM under the path spelling `game/hud` (which the registry resolves for `game:hud`), and publishes it. A Haxe call through a bound native reaches that path through `lookup_class_or_load`. A Wren `import "game:hud"` reaches it through the resolve callback, which returns the module's own name for a Wren module and installs a class for a module of another language.

Loading runs the module's top-level code, so it happens after the program is running and has published its classes. This is why the program publishes before it starts: the interpreter registers its closure runner as it starts, before its entry function runs, so a call from a Wren module's top level lands in a running program. A Wren module's plain imports are served from the same roots, starting with the importer's own directory.

## The Run Report

`Session::report` describes what the run did, in the program's own terms (`caribou::report`):

* Each Wren function of the project's modules, with the tier it reached and, when the session was opened with counting enabled, how often it was entered.
* Each Haxe method a tier compiled.
* Each site the program sends across the bridge from, whether the site holds a direct send, how many sends took the plain path, and whether the member links at AOT or stays dynamic (see [linking](linking.md)).
* How many scalars crossed boxed through a `Dynamic` parameter or result.
* How many closures crossed typed and how many crossed boxed.

Each adapter answers for its language (`caribou_ash::report`, `caribou_wren::report`), and the session gathers the answers. `caribou run --report` prints the report when the program ends.

## Reload

`World::reload(namespace, module)` loads a module again from the project's sources, whatever language it is. The adapter that serves the language re-runs the module in place (`Adapter::reload`): its classes keep their identity and get the new method bodies, and its interface is published again. The protocol's epoch is then bumped, so every call site in every language refills on its next use (see [bridge.md](bridge.md#call-sites)), and subscribers receive `Event::Reload`. An object of the module that was created before the reload keeps its class and therefore runs the new bodies. A call that another language bound to the class before the reload reaches the new bodies through its call site.

**WrenLift:** WrenLift reloads a module by re-running it while keeping the class objects it declared (`VM::reload_module`). It drops the module's compiled bodies and clears its inline caches. The adapter then publishes the module again and runs the program's `Hatch.onReload` callbacks. `Session::reload` does the same with the VM entered. Ash does not support reload yet: `Adapter::reload` returns an error for Haxe.

**Zyntax:** The adapter parses the module's source again, staged or from its file, and hands the typed program to the language's runtime (`TieredRuntime::reload_typed_program`). The runtime lowers it, compares each function with the running one, and swaps the code of the functions that changed; calls between the module's compiled functions go through cells, so the swap reaches them. The interface is then published again with the code now behind each symbol. A function that fails to compile keeps its old code and fails the reload. The runtime diffs an edit against the module it compiled last, so a language's last-loaded module is the one that reloads; the adapter refuses the others with an error that names the module in the way. A module's function that another language holds as a value follows too: the `Function` object a Wren module variable holds (`caribou::function::of_module`) reads the function from the module's interface again once the epoch has moved.

**Triggers:** A reload is triggered either by the driver calling `reload` or by the world's own file watch. `World::watch_sources` checks the file that every loaded module came from (`registry::sources`, which a language's loader records) on a short interval, from a thread of its own. When a file changes, the watch raises a reactor source (see [scheduler.md](scheduler.md#the-reactor)) whose handler reloads the module on the world's thread, between scheduler turns. In practice this means wherever the program is idle: a `Sys.sleep`, a `Lock` wait, or a frame's pacing. A module loaded later is watched from then on. A source that no longer compiles reloads nothing: the module stays as it was, and the event carries the error. A session watches from the moment it opens, and `caribou run` prints to stderr what it reloaded.

## Events

`World::on(kind, handler)` subscribes a handler to events of a kind, and `World::raise` raises one. Handlers run on the world's thread, from `tick` and at the end of a reload. They never run inside a collection or a stack switch, and they run with the world unborrowed, so a handler may subscribe, raise, or reload. `Reload` is the only event kind so far. It carries the error when the load failed. A task's uncaught error and a language's log line arrive as events through the paths that raise them.

## Current Implementation Boundaries

**Implemented:**

* The adapter registry, the language table, the namespace table, the source roots, and the loaders.
* The driver described above, and sessions opened from a bundle.
* Reload of a Wren module and of a Zyntax module, the file watch that triggers it, and events.

**Not yet implemented:**

* Reload for Ash.

Under a session, the program's own loop drives the scheduler, and the world's handlers run from there through the reactor. A driver with a loop of its own receives them from `tick` as well.
