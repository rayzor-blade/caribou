//! A program's plugin crates, built for the ways a program can take a
//! plugin: for this machine, to load in a hosted run; linked into a wasm
//! program's runtime; or as a wasm side module beside the program
//! ([`side_module`]).
//!
//! A program that links plugins in builds its runtime with them. Two Rust
//! staticlibs built apart would each carry std and an allocator, so the
//! program's plugins and `caribou-runtime` build as one crate graph: a
//! crate of the program's own in its target directory, depending on both,
//! with `caribou_abi`'s `linked` feature, under which each plugin exports
//! its entry under a name of its own, and a constructor of the crate
//! registers them. Every build here is from the caribou source this driver
//! was built from, with that source's lockfile, patches and profiles.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::wasm_toolchain;

/// The caribou source this driver was built from.
fn source_root() -> Result<PathBuf> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    root.join("crates/caribou-runtime/Cargo.toml")
        .is_file()
        .then(|| root.canonicalize())
        .transpose()?
        .ok_or_else(|| {
            anyhow!(
                "linking plugins into a program builds caribou's runtime from source, and \
                 the source this caribou was built from is gone: {}",
                root.display()
            )
        })
}

/// Whether `path` is a plugin's crate rather than its built library.
pub fn is_crate(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
}

/// A plugin crate's package name and library name, from its manifest.
fn names(dir: &Path) -> Result<(String, String)> {
    let manifest = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let table: toml::Table =
        toml::from_str(&text).with_context(|| manifest.display().to_string())?;
    let package = table
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("{} names no package", manifest.display()))?;
    let lib = table
        .get("lib")
        .and_then(|l| l.get("name"))
        .and_then(|n| n.as_str())
        .map_or_else(|| package.replace('-', "_"), str::to_owned);
    Ok((package.to_owned(), lib))
}

/// The plugin crate at `dir` built for this machine, as the loader opens
/// it: what a hosted run loads and what describes the plugin to a build.
/// Built under `target_dir`.
pub fn host_library(dir: &Path, target_dir: &Path) -> Result<PathBuf> {
    let root = source_root()?;
    // Cargo runs from the source root.
    let (dir, target_dir) = (std::path::absolute(dir)?, std::path::absolute(target_dir)?);
    let (_, lib) = names(&dir)?;
    let build_dir = target_dir.join("plugins");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(cargo)
        .args(["build", "--release", "--lib", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&build_dir)
        .current_dir(&root)
        .status()
        .context("running cargo")?;
    if !status.success() {
        bail!("building the plugin crate {}: {status}", dir.display());
    }
    let file = format!(
        "{}{lib}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let library = build_dir.join("release").join(file);
    if !library.is_file() {
        bail!(
            "{} built no {}: a plugin crate's crate-type lists cdylib, for hosted runs, and \
             rlib, to link into a program",
            dir.display(),
            library.display()
        );
    }
    Ok(library)
}

/// The plugin crate at `dir` built as a wasm side module for `triple`,
/// written to `out`: position-independent, over its own copy of std built
/// that way, with `caribou_abi`'s `side_module` feature, so it allocates
/// with the program's `malloc` and takes the host table from the program.
/// It exports `exports`, its members' link symbols, which the program
/// finds in it when it starts. Built under `target_dir`. Returns what the
/// plugin wrote for a page ([`page_files`]).
pub fn side_module(
    dir: &Path,
    triple: &str,
    exports: &[String],
    target_dir: &Path,
    out: &Path,
) -> Result<Vec<PathBuf>> {
    let root = source_root()?;
    let (dir, target_dir) = (std::path::absolute(dir)?, std::path::absolute(target_dir)?);
    let (_, lib) = names(&dir)?;
    let build_dir = target_dir.join("side-modules");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = std::process::Command::new(cargo);
    build
        .args([
            "rustc",
            "--release",
            "--lib",
            "--crate-type",
            "staticlib",
            "--target",
            triple,
        ])
        .args([
            "-Z",
            "build-std=std,panic_abort",
            "--features",
            "caribou_abi/side_module",
        ])
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&build_dir)
        .current_dir(&root)
        // In place of the config's flags for the target: a side module is
        // position-independent, and its globals are the program's to move.
        .env(
            format!(
                "CARGO_TARGET_{}_RUSTFLAGS",
                triple.replace('-', "_").to_uppercase()
            ),
            "-Crelocation-model=pic -Ctarget-feature=+mutable-globals",
        )
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    let out_dirs = build_reporting(&mut build).with_context(|| {
        format!(
            "building the plugin crate {} as a side module",
            dir.display()
        )
    })?;
    let archive = build_dir
        .join(triple)
        .join("release")
        .join(format!("lib{lib}.a"));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    wasm_toolchain::side_module(&rustc, &root, &archive, exports, out).map_err(|e| anyhow!(e))?;
    Ok(page_files(&out_dirs))
}

/// Run a cargo build, its messages read for the `OUT_DIR` of every build
/// script in it, ran or fresh.
fn build_reporting(build: &mut std::process::Command) -> Result<Vec<PathBuf>> {
    let out = build
        .arg("--message-format=json-render-diagnostics")
        .stdout(std::process::Stdio::piped())
        .output()
        .context("running cargo")?;
    if !out.status.success() {
        bail!("{}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|m| m["reason"] == "build-script-executed")
        .filter_map(|m| m["out_dir"].as_str().map(PathBuf::from))
        .collect())
}

/// What the plugins' builds wrote for a page: every file in an `OUT_DIR`'s
/// `page` directory, which the program ships beside the module. A plugin's
/// entry there is `<name>.mjs`, which the page imports on its own thread
/// when the plugin asks the host for its agent (see `docs/architecture/
/// plugins.md`).
fn page_files(out_dirs: &[PathBuf]) -> Vec<PathBuf> {
    out_dirs
        .iter()
        .flat_map(|dir| {
            std::fs::read_dir(dir.join("page"))
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect()
}

/// Build the runtime object for `triple` with `plugins` linked in, each
/// its crate and the name it registers under, under `target_dir`. Returns
/// the object, and what the plugins wrote for a page ([`page_files`]).
pub fn runtime(
    plugins: &[(PathBuf, String)],
    triple: &str,
    target_dir: &Path,
) -> Result<(PathBuf, Vec<PathBuf>)> {
    let root = source_root()?;
    let target_dir = std::path::absolute(target_dir)?;
    let sysroot = wasm_toolchain::sysroot(triple).ok_or_else(|| {
        anyhow!(
            "no WASI sysroot for {triple}: install wasi-libc or the WASI SDK, or set \
             WASI_SYSROOT"
        )
    })?;
    let dir = target_dir.join("linked");
    std::fs::create_dir_all(&dir)?;

    let mut deps = toml::Table::new();
    let path_dep = |path: &Path| {
        let mut t = toml::Table::new();
        t.insert("path".into(), path.display().to_string().into());
        t
    };
    deps.insert(
        "caribou-runtime".into(),
        path_dep(&root.join("crates/caribou-runtime")).into(),
    );
    let mut abi = path_dep(&root.join("crates/caribou_abi"));
    abi.insert("features".into(), vec![toml::Value::from("linked")].into());
    deps.insert("caribou_abi".into(), abi.into());
    deps.insert(
        "caribou-plugin".into(),
        path_dep(&root.join("crates/caribou-plugin")).into(),
    );
    // Each plugin's crate, and a constructor that registers its entry.
    let mut crates = String::from("extern crate caribou_runtime;\n");
    let mut entries = String::new();
    let mut calls = String::new();
    for (i, (plugin, name)) in plugins.iter().enumerate() {
        let dir = plugin
            .canonicalize()
            .with_context(|| format!("plugin crate {}", plugin.display()))?;
        let (package, lib) = names(&dir)?;
        crates.push_str(&format!("extern crate {lib};\n"));
        entries.push_str(&format!(
            "    #[link_name = {:?}]\n    fn entry_{i}(host: *const Host) -> *const PluginInfo;\n",
            caribou_abi::linked_entry_symbol(name)
        ));
        calls.push_str(&format!(
            "    caribou_plugin::register_linked(entry_{i});\n"
        ));
        deps.insert(package, path_dep(&dir).into());
    }
    let lib = format!(
        "{crates}
use caribou_abi::PluginInfo;
use caribou_abi::host::Host;

unsafe extern \"C\" {{
{entries}}}

#[used]
#[unsafe(link_section = \".init_array\")]
static REGISTER: extern \"C\" fn() = register;

extern \"C\" fn register() {{
{calls}}}
"
    );

    let (patch, profile) = source_tables(&root)?;
    let mut manifest: toml::Table = toml::from_str(
        r#"
[package]
name = "caribou-program"
version = "0.0.0"
edition = "2024"
publish = false

[lib]
path = "lib.rs"
crate-type = ["staticlib"]

[workspace]
"#,
    )?;
    manifest.insert("dependencies".into(), deps.into());
    if let Some(patch) = patch {
        manifest.insert("patch".into(), patch);
    }
    if let Some(profile) = profile {
        manifest.insert("profile".into(), profile);
    }
    std::fs::write(dir.join("Cargo.toml"), toml::to_string(&manifest)?)?;
    std::fs::write(dir.join("lib.rs"), lib)?;
    // The source's versions, to which cargo adds the plugins' own.
    std::fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock"))?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let build_dir = target_dir.join("linked-build");
    let mut build = wasm_toolchain::cargo_build(&cargo, &root, triple, &sysroot, &build_dir);
    build.arg("--manifest-path").arg(dir.join("Cargo.toml"));
    let out_dirs = build_reporting(&mut build)
        .with_context(|| format!("building the program's runtime with its plugins for {triple}"))?;
    let archive = build_dir.join(triple).join("release/libcaribou_program.a");
    let object = target_dir.join(triple).join("caribou_runtime.o");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    wasm_toolchain::prelink(&rustc, &root, &archive, &sysroot, triple, &object)
        .map_err(|e| anyhow!(e))?;
    Ok((object, page_files(&out_dirs)))
}

/// The source workspace's `[patch]` and `[profile]` tables, with each
/// patch's path made absolute: a crate outside the workspace reads
/// neither.
fn source_tables(root: &Path) -> Result<(Option<toml::Value>, Option<toml::Value>)> {
    let path = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&path)?;
    let mut table: toml::Table =
        toml::from_str(&text).with_context(|| path.display().to_string())?;
    let mut patch = table.remove("patch");
    if let Some(sources) = patch.as_mut().and_then(|p| p.as_table_mut()) {
        for crates in sources.iter_mut().filter_map(|(_, s)| s.as_table_mut()) {
            for dep in crates.iter_mut().filter_map(|(_, d)| d.as_table_mut()) {
                if let Some(p) = dep.get("path").and_then(|p| p.as_str()) {
                    let absolute = root.join(p).display().to_string();
                    dep.insert("path".into(), absolute.into());
                }
            }
        }
    }
    Ok((patch, table.remove("profile")))
}

/// Join relocatable wasm objects into `out`: a runtime object and what the
/// program links beside it.
pub fn join(objects: &[&Path], out: &Path) -> Result<()> {
    let root = source_root()?;
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    wasm_toolchain::join(&rustc, &root, objects, out).map_err(|e| anyhow!(e))
}
