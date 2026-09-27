//! A program built ahead of time: Ash's AOT build of the `.hl`, linked
//! against caribou's runtime in place of Ash's own. Other language
//! frontends contribute relocatable objects to the same Ash link. No
//! language interpreter or source-module loader participates.
//!
//! Only wasm for now: the wasm runtime object is built with the driver
//! (`build.rs`), and a release puts it beside the binary as Ash's does.

use std::path::{Path, PathBuf};

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use ash_core::llvm::aot_build::{AotRequest, emit_aot};
use ash_core::llvm::aot_link::is_wasm_triple;
use ash_core::native_lib::{HostLink, Word};
use caribou_abi::TypeTag;

use crate::{aot_languages, linked};

/// Build `program` for `triple` into `out`, by default the program's name
/// with the target's extension beside it. Plugin crates use the linked
/// compatibility path, while language frontends contribute objects to Ash's
/// final link. Build artifacts live under `target_dir` (by default `target/`
/// beside the output). Returns what was written.
pub fn build(
    program: &Path,
    triple: &str,
    out: Option<&Path>,
    plugins: &[PathBuf],
    sources: &[PathBuf],
    target_dir: Option<&Path>,
) -> Result<PathBuf> {
    build_with_languages(program, triple, out, plugins, sources, &[], target_dir)
}

/// Build with the languages declared by a project. A frontend without a
/// relocatable-object emitter is reported before the final link.
pub fn build_with_languages(
    program: &Path,
    triple: &str,
    out: Option<&Path>,
    plugins: &[PathBuf],
    sources: &[PathBuf],
    languages: &[String],
    target_dir: Option<&Path>,
) -> Result<PathBuf> {
    if !is_wasm_triple(triple) {
        bail!("`{triple}`: caribou builds wasm programs ahead of time so far");
    }
    let exe = out.map_or_else(|| program.with_extension("wasm"), Path::to_path_buf);
    let target_dir = target_dir.map_or_else(
        || exe.parent().unwrap_or(Path::new(".")).join("target"),
        Path::to_path_buf,
    );
    let (crates, libraries): (Vec<PathBuf>, Vec<PathBuf>) =
        plugins.iter().cloned().partition(|p| linked::is_crate(p));
    // A library joins a wasm program only as a side module.
    let unlinkable = links(program, &libraries, None)?;
    if let Some((_, name)) = unlinkable.keys().next() {
        bail!(
            "`{name}` is a native plugin member; a wasm program requires its plugin as an Ash \
             side module; see git-bug issue `wasm: plugins load as Ash side modules`"
        );
    }
    let described = crates
        .iter()
        .map(|c| linked::host_library(c, &target_dir))
        .collect::<Result<Vec<_>>>()?;
    let runtime = if crates.is_empty() {
        wasm_runtime(triple)?
    } else {
        let mut named = Vec::with_capacity(crates.len());
        for (dir, library) in crates.iter().zip(&described) {
            let plugin = caribou_plugin::load(library).map_err(|e| anyhow!("{e}"))?;
            named.push((dir.clone(), plugin.name().to_owned()));
        }
        linked::runtime(&named, triple, &target_dir)?
    };
    let languages = aot_languages::Artifacts::build(
        languages,
        sources,
        triple,
        &target_dir.join(triple).join("languages"),
    )?;
    // Scratch, named after the module beside it and removed once linked.
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".o");
    let object = exe.with_file_name(name);
    emit_aot(AotRequest {
        file: program,
        out: &object,
        exe: Some(&exe),
        runtime: Some(&runtime),
        target: Some(triple.to_owned()),
        pgo: None,
        allow_refused: false,
        abi_version: 1,
        quiet: false,
        links: links(program, &described, Some(&languages))?,
        objects: languages.objects(),
    })?;
    Ok(exe)
}

/// Caribou's runtime object for `triple`: beside the binary, where a
/// release puts it, else the one this build of the driver made.
fn wasm_runtime(triple: &str) -> Result<PathBuf> {
    const NAME: &str = "caribou_runtime.o";
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(triple).join(NAME)));
    let built = option_env!("CARIBOU_WASM_RUNTIME").map(PathBuf::from);
    beside
        .into_iter()
        .chain(built)
        .find(|p| p.is_file())
        .ok_or_else(|| {
            anyhow!(
                "no {NAME} for {triple}: this caribou was built without a WASI sysroot. \
                 Install wasi-libc or the WASI SDK (or set WASI_SYSROOT) and rebuild it"
            )
        })
}

/// The program's `caribou` natives that a plugin or a linked Wren module
/// defines, each linked to the member by its symbol: the callee's own
/// machine types, and the casts between them and Haxe's. A member whose
/// types have no cast yet is left to the program's run-time path.
fn links(
    program: &Path,
    plugins: &[PathBuf],
    languages: Option<&aot_languages::Artifacts>,
) -> Result<HashMap<(String, String), HostLink>> {
    let mut members = Vec::new();
    for plugin in plugins {
        members.extend(caribou_plugin::links(plugin).map_err(|e| anyhow!("{e}"))?);
    }
    let mut out = HashMap::new();
    for (lib, name) in caribou_ash::program::natives_in(program)? {
        if lib != "caribou" {
            continue;
        }
        let Some(m) = caribou_ash::link::member_of(&name) else {
            continue;
        };
        let found = members.iter().find(|l| {
            l.lang == m.namespace
                && l.module == m.module
                && l.class == m.class
                && l.kind == m.kind
                && l.name == m.name
                && l.arity == m.arity
        });
        let link = match found {
            Some(found) => host_link(found),
            None => languages.and_then(|languages| languages.host_link(&m)),
        };
        if let Some(link) = link {
            out.insert((lib, name), link);
        }
    }
    Ok(out)
}

/// A plugin member as Ash links it, or `None` while one of its types has
/// no cast: a number or a bool passes as it is, a string through the
/// core's string, and whatever the plugin raises is thrown after.
fn host_link(link: &caribou_plugin::Link) -> Option<HostLink> {
    let mut params = Vec::with_capacity(link.params.len());
    let mut arg_casts = Vec::with_capacity(link.params.len());
    for &tag in &link.params {
        let (word, cast) = word_of(tag)?;
        params.push(word);
        arg_casts.push(cast.map(|(to_plugin, _)| to_plugin.to_owned()));
    }
    let (ret, ret_cast) = if link.ret == TypeTag::VOID {
        (None, None)
    } else {
        let (word, cast) = word_of(link.ret)?;
        (Some(word), cast.map(|(_, to_haxe)| to_haxe.to_owned()))
    };
    Some(HostLink {
        symbol: link.symbol.clone(),
        params,
        ret,
        arg_casts,
        ret_cast,
        after: Some("caribou_haxe_raise_pending".to_owned()),
        init: None,
    })
}

/// The machine word a plugin tag is, with the casts into and out of it
/// from Haxe's side when the two differ.
fn word_of(tag: TypeTag) -> Option<(Word, Option<(&'static str, &'static str)>)> {
    Some(match tag {
        TypeTag::UI8 | TypeTag::UI16 | TypeTag::I32 => (Word::I32, None),
        TypeTag::I64 => (Word::I64, None),
        TypeTag::F32 => (Word::F32, None),
        TypeTag::F64 => (Word::F64, None),
        TypeTag::BOOL => (Word::Bool, None),
        TypeTag::BYTES => (
            Word::Ptr,
            Some(("caribou_haxe_string_to_str", "caribou_haxe_str_to_string")),
        ),
        _ => return None,
    })
}


/// Run the wasm module at `module` with `args`, as `ash run` does: under
/// wasmtime, with Ash's host supplying what WASI does not. Returns its
/// exit status.
pub fn run_module(module: &Path, args: &[String]) -> Result<i32> {
    use ash_wasm_runtime::native::{Outcome, Program};

    let program = Program::load(module)?;
    let name = module
        .file_name()
        .map_or_else(|| "program".to_owned(), |n| n.to_string_lossy().into_owned());
    let argv: Vec<String> = std::iter::once(name).chain(args.iter().cloned()).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Threads are started only for a module built for them.
    Ok(match runtime.block_on(program.run(&argv, &[], true))? {
        Outcome::Exited(code) => code,
        Outcome::Trapped(trap) => {
            eprintln!("{trap}");
            70
        }
    })
}
