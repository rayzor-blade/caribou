//! A program built ahead of time: Ash's AOT build of the `.hl`, linked
//! against caribou's runtime in place of Ash's own. Other language
//! frontends contribute relocatable objects to the same Ash link. No
//! language interpreter or source-module loader participates.
//!
//! Only wasm for now: the wasm runtime object is built with the driver
//! (`build.rs`), and a release puts it beside the binary as Ash's does.

use std::path::{Path, PathBuf};

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
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
    build_with_languages(program, triple, out, plugins, &[], sources, &[], target_dir)
}

/// Build with the languages declared by a project. A frontend without a
/// relocatable-object emitter is reported before the final link. The
/// plugin crates among `side_modules` are built as side modules beside the
/// output, where the program finds their members when it starts; the rest
/// are linked in.
pub fn build_with_languages(
    program: &Path,
    triple: &str,
    out: Option<&Path>,
    plugins: &[PathBuf],
    side_modules: &[PathBuf],
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
    let (side, crates): (Vec<PathBuf>, Vec<PathBuf>) =
        crates.into_iter().partition(|c| side_modules.contains(c));
    // A library joins a wasm program only as a side module.
    let unlinkable = links(program, &unlinked(&libraries), None)?;
    if let Some((_, name)) = unlinkable
        .keys()
        .find(|(_, name)| library_native(name).is_none())
    {
        bail!(
            "`{name}` is a native plugin member; a wasm program requires its plugin as an Ash \
             side module; see git-bug issue `wasm: plugins load as Ash side modules`"
        );
    }
    let described = crates
        .iter()
        .map(|c| linked::host_library(c, &target_dir))
        .collect::<Result<Vec<_>>>()?;
    let (runtime, mut agents) = if crates.is_empty() {
        (wasm_runtime(triple, &target_dir)?, Vec::new())
    } else {
        let mut named = Vec::with_capacity(crates.len());
        for (dir, library) in crates.iter().zip(&described) {
            let plugin = caribou_plugin::load(library).map_err(|e| anyhow!("{e}"))?;
            named.push((dir.clone(), plugin.name().to_owned()));
        }
        linked::runtime(&named, triple, &target_dir)?
    };
    // Each side module beside the output, under its plugin's name, which is
    // the library its members are found in.
    let mut described = unlinked(&described);
    for dir in &side {
        let library = linked::host_library(dir, &target_dir)?;
        let plugin = caribou_plugin::load(&library).map_err(|e| anyhow!("{e}"))?;
        let name = plugin.name().to_owned();
        let exports: Vec<String> = caribou_plugin::links(&library)
            .map_err(|e| anyhow!("{e}"))?
            .into_iter()
            .map(|l| l.symbol)
            .chain(
                [
                    caribou_abi::PLUGIN_ENTRY_SYMBOL,
                    caribou_abi::ABI_VERSION_SYMBOL,
                ]
                .map(String::from),
            )
            .collect();
        let module = exe.with_file_name(format!("{name}.wasm"));
        agents.extend(linked::side_module(
            dir,
            &name,
            triple,
            &exports,
            &target_dir,
            &module,
        )?);
        described.push((library, Some(name)));
    }
    // What a page starts beside the program when a plugin asks for its agent.
    for agent in &agents {
        let beside = exe.with_file_name(agent.file_name().unwrap_or_default());
        std::fs::copy(agent, &beside).with_context(|| format!("writing {}", beside.display()))?;
    }
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
/// release puts it; the one this build of the driver made, which is for
/// `wasm32-wasip1`; else built for the program under `target_dir`, from the
/// source this driver was built from.
fn wasm_runtime(triple: &str, target_dir: &Path) -> Result<PathBuf> {
    const NAME: &str = "caribou_runtime.o";
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(triple).join(NAME)));
    let built = option_env!("CARIBOU_WASM_RUNTIME")
        .filter(|_| triple == "wasm32-wasip1")
        .map(PathBuf::from);
    if let Some(found) = beside.into_iter().chain(built).find(|p| p.is_file()) {
        return Ok(found);
    }
    if triple != "wasm32-wasip1" {
        return linked::runtime(&[], triple, target_dir).map(|(object, _)| object);
    }
    Err(anyhow!(
        "no {NAME} for {triple}: this caribou was built without a WASI sysroot. \
         Install wasi-libc or the WASI SDK (or set WASI_SYSROOT) and rebuild it"
    ))
}

/// Plugin libraries whose members are linked into the program.
fn unlinked(libraries: &[PathBuf]) -> Vec<(PathBuf, Option<String>)> {
    libraries.iter().map(|l| (l.clone(), None)).collect()
}

/// The program's `caribou` natives that a plugin or a linked Wren module
/// defines, each linked to the member by its symbol: the callee's own
/// machine types, and the casts between them and Haxe's. A member whose
/// types have no cast yet is left to the program's run-time path.
fn links(
    program: &Path,
    plugins: &[(PathBuf, Option<String>)],
    languages: Option<&aot_languages::Artifacts>,
) -> Result<HashMap<(String, String), HostLink>> {
    // Each member with the side module it is in, if it is in one.
    let mut members = Vec::new();
    for (plugin, library) in plugins {
        let links = caribou_plugin::links(plugin).map_err(|e| anyhow!("{e}"))?;
        members.extend(links.into_iter().map(|l| (l, library.clone())));
    }
    let mut out = HashMap::new();
    for (lib, name) in caribou_ash::program::natives_in(program)? {
        if lib != "caribou" {
            continue;
        }
        let Some(m) = caribou_ash::link::member_of(&name) else {
            if let Some(link) = library_native(&name) {
                out.insert((lib, name), link);
            }
            continue;
        };
        let found = members.iter().find(|(l, _)| {
            l.lang == m.namespace
                && l.module == m.module
                && l.class == m.class
                && l.kind == m.kind
                && l.name == m.name
                && l.arity == m.arity
        });
        let link = match found {
            Some((found, library)) => host_link(found).map(|link| HostLink {
                library: library.clone(),
                ..link
            }),
            None => languages.and_then(|languages| languages.host_link(&m)),
        };
        if let Some(link) = link {
            out.insert((lib, name), link);
        }
    }
    Ok(out)
}

/// One of the caribou library's own natives (`caribou.Future`,
/// `caribou.Sequence`), linked to its entry in `caribou_ash::link`.
fn library_native(name: &str) -> Option<HostLink> {
    use Word::{Bool, I32, Ptr};
    let text = || Some("caribou_haxe_string_to_str".to_owned());
    let (params, ret): (&[Word], Option<Word>) = match name {
        "face" => (&[Ptr, Ptr, Ptr, Ptr], None),
        "future_new" => (&[Ptr], None),
        "future_ready" => (&[Ptr], Some(Bool)),
        "future_await" => (&[Ptr], Some(Ptr)),
        "future_resolve" | "future_reject" => (&[Ptr, Ptr], Some(Bool)),
        "len" => (&[Ptr], Some(I32)),
        "index" => (&[Ptr, I32], Some(Ptr)),
        "set_index" => (&[Ptr, I32, Ptr], None),
        _ => return None,
    };
    Some(HostLink {
        symbol: format!("caribou_haxe_{name}"),
        params: params.to_vec(),
        ret,
        // The class a face names itself by, as core strings.
        arg_casts: if name == "face" {
            vec![text(), text(), text(), None]
        } else {
            vec![None; params.len()]
        },
        ret_cast: None,
        after: Some("caribou_haxe_raise_pending".to_owned()),
        init: None,
        library: None,
    })
}

/// A plugin member as Ash links it, or `None` while one of its types has
/// no cast: a number or a bool passes as it is, a string through the
/// core's string, an instance as its payload behind the Haxe face, a
/// constructor binding the face Haxe allocated; whatever the plugin raises
/// is thrown after.
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
    // A constructor binds the face Haxe allocated to the object it makes.
    let (ret_cast, init) = if link.kind == caribou::link::Kind::Constructor {
        (None, Some("caribou_plugin_bind_face".to_owned()))
    } else {
        (ret_cast, None)
    };
    Some(HostLink {
        symbol: link.symbol.clone(),
        params,
        ret,
        arg_casts,
        ret_cast,
        after: Some("caribou_haxe_raise_pending".to_owned()),
        init,
        library: None,
    })
}

/// The machine word a plugin tag is, with the casts into and out of it
/// from Haxe's side when the two differ.
fn word_of(tag: TypeTag) -> Option<(Word, Option<(&'static str, &'static str)>)> {
    Some(match tag {
        TypeTag::UI8 | TypeTag::UI16 | TypeTag::I32 => (Word::I32, None),
        TypeTag::I64 => (Word::I64, None),
        // Haxe has only a double.
        TypeTag::F32 => (
            Word::F32,
            Some(("caribou_haxe_f64_to_f32", "caribou_haxe_f32_to_f64")),
        ),
        TypeTag::F64 => (Word::F64, None),
        TypeTag::BOOL => (Word::Bool, None),
        TypeTag::BYTES => (
            Word::Ptr,
            Some(("caribou_haxe_string_to_str", "caribou_haxe_str_to_string")),
        ),
        // Bytes shared both ways, as a core buffer over Haxe's own.
        TypeTag::BUFFER | TypeTag::BUFFER_MUT => (
            Word::Ptr,
            Some((
                "caribou_haxe_bytes_to_buffer",
                "caribou_haxe_buffer_to_bytes",
            )),
        ),
        TypeTag::ENUM => (
            Word::Ptr,
            Some((
                "caribou_plugin_enum_from_haxe",
                "caribou_plugin_enum_to_haxe",
            )),
        ),
        // Any value, as the core's.
        TypeTag::DYN => (
            Word::I64,
            Some(("caribou_haxe_dyn_to_value", "caribou_haxe_value_to_dyn")),
        ),
        // A future: the core's, behind a `caribou.Future`.
        TypeTag::FUTURE => (
            Word::Ptr,
            Some((
                "caribou_haxe_future_from_haxe",
                "caribou_haxe_future_to_haxe",
            )),
        ),
        // An instance: its payload to the plugin, its face to Haxe.
        TypeTag::OBJ => (
            Word::Ptr,
            Some((
                "caribou_plugin_from_haxe_face",
                "caribou_plugin_to_haxe_face",
            )),
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
    let name = module.file_name().map_or_else(
        || "program".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
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
