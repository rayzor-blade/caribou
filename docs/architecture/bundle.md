# Bundle

A bundle is a program and the modules of every language it uses in one
file. `caribou build` writes one from a project's layout; `caribou run`
opens it where it would open the program, and a session from a bundle
sees what a session from the directory saw. The bundle is what ships:
a run needs the file and nothing beside it.

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
bundle: a Haxe program is `hl`; a Wren module is `wlbc@N`, wren_lift's
compiled form at the version `N` of its serializer, or `source`, which
the adapter also reads; a hatch package the project depends on is
`hatch`, whole, under its name (`@hatch:noise`), its own native
libraries inside it. The bundle versions its framing alone. A *source* section is
the text a compiled module was built from, under the module's name, for
the language's diagnostics. A *resource* section is bytes by name. A
*native library* section is a plugin on the shared ABI, under its file
name, for the target its format names, `<arch>-<os>` as the standard
library spells them (`aarch64-macos`); a bundle may carry one per
target, and a run takes its own target's.

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
Wren modules are compiled (`caribou_wren::project::compile`) on one
VM, each after the modules it imports by a plain import
(`import_order`), so a class one module declares is known to the
modules that use it, and each becomes a `wlbc@N` section with its text
beside it as a source section. A module
that does not compile fails the build, naming it. The plugins in
`plugins/` beside the program, the ones the run from the directory
loaded, go in as native library sections for the building machine's
target. The bundle is written beside the program as `game.cb`, or where
`-o` says.

## Opening

`Session::open` reads the file's first bytes: a bundle is opened as one,
anything else as a program. From a bundle the program is loaded from
its section's bytes (`caribou_ash::load_bytes`, ash's
`BytecodeDecoder::decode_bytes`), with the bundle's path standing for
the program's: the libraries beside it, its `argv[0]`, its tier's cache.
The world is made from the manifest's namespaces with no source root,
and `World::install` hands every other module section to the adapter
of its language (`Adapter::install`). The Wren adapter *stages* a
module under its name, compiled or as source: the loader looks among
the staged modules before the roots, so the module loads on first use
as it would from a file, by `interpret_bytecode` or `interpret`. A
module's own plain imports resolve among them the same way, through
wren_lift's `load_bytecode_fn` beside its `load_module_fn`; a compiled
module loads what it imports as a source module does. An adapter
refuses a format it does not read, a `wlbc` of another version
included, and the open fails then, naming the section.

A source section beside a compiled module is the text it was built
from: the adapter keeps it with the module, and gives it to the VM as
the module loads (`interpret_bytecode_with_source`), so a runtime error
in the module renders its line as it would from a file. The Haxe
program's own lines come with its bytecode, when it was built with
debug information.

A bundle's plugins are its native library sections for the running
target, not a `plugins/` beside it: a library loads from a file, so each
is written once under the temporary directory by the hash of its bytes
and loaded from there, as a program's are loaded from `plugins/`
(`caribou_driver::bundle::plugins`).

Nothing is watched: a bundle's modules have no file to change.
`Session::reload` re-runs a module staged as source from what was
staged; a compiled one does not reload.

## Boundaries of the current implementation

Built: the format, `build`, opening, Haxe `hl` entries, Wren modules
compiled or as source, hatch packages, native libraries for the
building target. Not built: resources reachable from a program, the Api sections (the registry's interfaces beside the modules,
for a build or an editor that reads the bundle without loading it),
native libraries for other targets than the building machine's, docs,
and compression.
