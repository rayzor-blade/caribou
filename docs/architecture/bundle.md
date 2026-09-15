# Bundle

A bundle is a program and the modules of every language it uses in one
file. `caribou build` writes one from a project's layout; `caribou run`
opens it where it would open the program, and a session from a bundle
sees what a session from the directory saw. The bundle is what ships:
a run needs the file and nothing beside it but the native libraries the
program already needed.

## What is in it

The manifest names the bundle, its entry module and the project's
namespaces (see [registry.md](registry.md#namespaces)). The entry is
the module the driver starts, a Haxe `hl` program today. The namespaces
are what `project::namespaces` gave the world at build time: every
directory under a root, and every namespace the program imports, each
over every resident language.

Then the sections. A *module* section is a language name, the format of
its bytes, the module's name in its language and the bytes. The format
is the language's own and versioned by the language, never by the
bundle: a Haxe program is `hl`, a Wren module is `source` today and
`wlbc@7` or a `.hatch` package when the adapter reads them. The bundle
versions its framing alone. A *resource* section is bytes by name.

The file is the magic `CARIBOU\0`, a version, flags, the manifest, then
the sections, every integer little-endian and every string and byte
string length-prefixed (`caribou::bundle`). No flag is defined; a set
one is refused, so a later bundle that needs something this reader
lacks fails at once rather than half-runs.

## Building

`caribou build game.hl` reads the project as `run` does: the roots are
the class paths of the `.hxml` files (`project::roots`), the imported
namespaces come from the program's `caribou` natives, read from the
bytecode without loading it (`caribou_ash::imports_in`), and every
`.wren` under a root becomes a module section named by its path under
the root, `game/hud`. Where two roots have a module of one name, the
first root's is taken, as a run from the directory would take it. The
bundle is written beside the program as `game.cb`, or where `-o`
says.

## Opening

`Session::open` reads the file's first bytes: a bundle is opened as one,
anything else as a program. From a bundle the program is loaded from
its section's bytes (`caribou_ash::load_bytes`, ash's
`BytecodeDecoder::decode_bytes`), with the bundle's path standing for
the program's: the libraries beside it, its `argv[0]`, its tier's cache.
The world is made from the manifest's namespaces with no source root,
and `World::install` hands every other module section to the adapter
of its language (`Adapter::install`). The Wren adapter *stages* a
source under its module name: the loader looks among the staged sources
before the roots, so the module loads on first use as it would from a
file, and a module's own plain imports resolve among them the same way.
An adapter refuses a format it does not read, and the open fails then,
naming the section.

Nothing is watched: a bundle's modules have no file to change. A reload
through `Session::reload` still works, and re-runs the staged source.

## Boundaries of the current implementation

Built: the format, `build`, opening, Haxe `hl` entries, Wren `source`
modules. Not built: Wren modules as `wlbc` or `.hatch` packages,
resources reachable from a program, the Api sections (the registry's
interfaces beside the modules, for a build or an editor that reads the
bundle without loading it), native libraries per target, docs, and
compression.
