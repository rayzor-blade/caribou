# Bundle Format & Deployment

## Overview

A bundle packages a program and the modules of every language it uses into one file. `caribou build` writes a bundle from a project's layout. `caribou run` opens a bundle the same way it opens a program, and a session started from a bundle sees the same thing a session started from the project directory would see. The bundle is the deployment artifact: a run needs the file and nothing else.

## Bundle Contents

**Manifest:** The manifest names the bundle, its entry module, and the project's namespaces (see [registry.md](registry.md#namespaces)). The entry is the module the driver starts. Today this is a Haxe `hl` program. The namespaces are what `project::namespaces` gave the world at build time: every directory under a root and every namespace the program imports, each covering every resident language.

**Sections:** The manifest is followed by the sections:

* **Module:** A language name, the format of the bytes, the module's name in its language, and the bytes. The format belongs to the language and is versioned by the language, never by the bundle. A Haxe program is `hl`. A Wren module is `wlbc@N` (WrenLift's compiled form, at version `N` of its serializer) or `source`, which the adapter also reads. A hatch package that the project depends on is `hatch`, stored whole under its name (`@hatch:noise`), with its own native libraries inside. A module of a Zyntax language is `source`, under its path below the root (`game/tally.py`), because its frontend parses it at load time the same way it parses a file. The bundle versions only its own framing.
* **Source:** The text that a compiled module was built from, under the module's name. The language uses it for diagnostics.
* **Resource:** Bytes stored by name.
* **Native library:** A library a run opens from a file, stored under its file name, for the target that its format names. The target is written `<arch>-<os>` using the standard library's names, for example `aarch64-macos`. A plugin on the shared ABI has an empty language; a Zyntax `.zrtl` plugin names `zyntax`. A bundle may carry one library per target, and a run takes the ones for its own target.
* **Language:** A frontend the run brings up itself to read the bundle's modules, under the language's name. The format says how it comes: `zsnap` is a Zyntax snapshot that carries the grammar, and `builtin` names a frontend built into caribou, such as Python. A bundle that needs a built-in frontend this build does not have fails to open, and the error names the language.

**Wire format:** The file consists of the magic bytes `CARIBOU\0`, a version, flags, the manifest, and then the sections. Every integer is little-endian, and every string and byte string is length-prefixed (`caribou::bundle`). No flag is defined yet. A set flag is rejected, so a future bundle that needs something this reader does not support fails immediately instead of running halfway.

## Building

`caribou build game.hl` reads the project the same way `run` does:

* **Roots:** The class paths of the `.hxml` files (`project::roots`).
* **Namespaces:** The imported namespaces come from the program's `caribou` natives, which are read from the bytecode without loading it (`caribou_ash::imports_in`).
* **Wren modules:** Every `.wren` file under a root becomes a module section, named by its path under the root, for example `game/hud`. If two roots have a module with the same name, the first root's module is used, as it would be in a run from the directory. The Wren modules are compiled on one VM (`caribou_wren::project::compile`), each after the modules it imports with a plain import (`import_order`), so a class one module declares is known to the modules that use it. Each module becomes a `wlbc@N` section, with its text stored next to it as a source section. A module that does not compile fails the build, and the error names the module.
* **Hatch packages:** The packages that the roots' hatchfiles depend on are stored whole. WrenLift reads its own format, native libraries included.
* **Zyntax languages:** Every frontend file directly under a root (`zynml.zsnap`, or a `.zyn` grammar, which is compiled and wrapped in a snapshot) becomes a language section, and Python becomes a `builtin` language section when a root holds a `.py` file. This is the same rule a run from the directory uses to register the languages. Each language is followed by the files its layout reads under the roots, one `source` module section per file, named by its path under the root. The first root's file wins, as in a run from the directory.
* **Plugins:** The plugins in `plugins/` next to the program, which are the ones a run from the directory would load, are stored as native library sections for the building machine's target, and so are the `.zrtl` plugins beside them.

The bundle is written next to the program as `game.cb`, or to the path given with `-o`.

## Opening

`Session::open` reads the first bytes of the file. A bundle is opened as a bundle; anything else is opened as a program.

**Program loading:** The program is loaded from its section's bytes (`caribou_ash::load_bytes`, which calls Ash's `BytecodeDecoder::decode_bytes`). The bundle's path stands in for the program's path: it determines the libraries next to it, its `argv[0]`, and its tier's cache location. The world is created from the manifest's namespaces with no source root, and `World::install` passes every other module section to the adapter for its language (`Adapter::install`).

**Module staging:** The Wren adapter *stages* each module under its name, whether compiled or source. The loader checks the staged modules before the roots, so the module loads on first use as it would from a file, through `interpret_bytecode` or `interpret`. A module's own plain imports resolve among the staged modules the same way, through WrenLift's `load_bytecode_fn` alongside its `load_module_fn`, so a compiled module loads its imports the same way a source module does. A hatch package is held until the VM exists and then staged into it. An adapter rejects a format it cannot read, including a `wlbc` of a different version. The open then fails, and the error names the section.

**Source sections:** A source section next to a compiled module contains the text the module was built from. The adapter keeps the text with the module and passes it to the VM when the module loads (`interpret_bytecode_with_source`), so a runtime error in the module shows its source line as it would when loaded from a file. The Haxe program's own source lines come with its bytecode when it was built with debug information.

**Zyntax languages:** The driver reads the language sections before it builds the world, because a language has to be registered before the world can hand it a module (`caribou_driver::bundle::frontends`). A `zsnap` language comes up from the snapshot's bytes, a `builtin` one from the frontend of that name in this build (`project::builtin`). The Zyntax adapter stages each `source` module under its name (`Adapter::install`). The loader maps a module name to the paths the language's layout gives it and checks the staged sources before the roots, so `game/tally` finds the staged `game/tally.py` the way it would find the file. A frontend that reads the modules a module imports, as Python does for `import game.util`, reads them through the same lookup (`caribou_zyntax::Sources`).

**Plugins:** A bundle's plugins are its native library sections for the running target, not a `plugins/` directory next to the bundle. A library has to be loaded from a file, so the sections are written once to one directory under the temporary directory, named by the hash of their contents, and loaded from there the same way a program's plugins are loaded from `plugins/` (`caribou_driver::bundle::native_libs`). The same directory is where the bundle's Zyntax frontends open their `.zrtl` plugins.

**Reload:** Nothing is watched, because a bundle's modules have no files that could change. `Session::reload` re-runs a module that was staged as source, using the staged text. A compiled module does not reload.

## Current Implementation Boundaries

**Implemented:**

* The format, `build`, and opening.
* Haxe `hl` entries.
* Wren modules, compiled or as source.
* Hatch packages.
* Native libraries for the building machine's target.
* Zyntax frontends as language sections, and their modules as source.

**Not yet implemented:**

* Resources that a program can access.
* API sections: the registry's interfaces stored next to the modules, for a build or an editor that reads the bundle without loading it.
* Native libraries for targets other than the building machine's.
* Zyntax modules in a compiled form. Zyntax's snapshot can carry a module lowered to HIR with its declarations beside it; a bundle could carry a project's modules that way once the frontends settle.
* Documentation sections and compression.
