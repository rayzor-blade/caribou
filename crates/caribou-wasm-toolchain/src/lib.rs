//! Host-neutral discovery of a WASI sysroot and a clang capable of Caribou wasm builds.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Environment variables which affect toolchain discovery.
pub const CONFIG_ENV: &[&str] = &[
    "WASI_SYSROOT",
    "WASI_SDK_PATH",
    "WASI_SDK_ROOT",
    "CARIBOU_WASM_CLANG",
    "WASI_CLANG",
    "CLANG",
    "PATH",
    "PATHEXT",
];

const SDK_ENV: [&str; 2] = ["WASI_SDK_PATH", "WASI_SDK_ROOT"];
const CLANG_ENV: [&str; 3] = ["CARIBOU_WASM_CLANG", "WASI_CLANG", "CLANG"];

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|candidate| candidate == &path) {
        paths.push(path);
    }
}

fn sdk_sysroot_candidates(paths: &mut Vec<PathBuf>, sdk: PathBuf) {
    push_unique(paths, sdk.join("share/wasi-sysroot"));
    // Also accept an environment variable that already names the sysroot.
    push_unique(paths, sdk);
}

fn compiler_sysroot_candidates(paths: &mut Vec<PathBuf>, compiler: &Path) {
    let Some(bin) = compiler.parent() else {
        return;
    };
    let Some(sdk) = bin.parent() else {
        return;
    };
    sdk_sysroot_candidates(paths, sdk.to_owned());
}

fn program_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let extensions: Vec<_> = env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_else(|| vec![String::new()]);

    for directory in env::split_paths(&path) {
        let plain = directory.join(name);
        if plain.is_file() {
            return Some(plain);
        }
        for extension in &extensions {
            if extension.is_empty() {
                continue;
            }
            let candidate = directory.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn package_prefix(package: &str) -> Option<PathBuf> {
    let manager = program_in_path("brew")?;
    let output = Command::new(manager)
        .args(["--prefix", package])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = String::from_utf8(output.stdout).ok()?;
    let prefix = prefix.trim();
    (!prefix.is_empty()).then(|| PathBuf::from(prefix))
}

fn library_targets(target: &str) -> Vec<&str> {
    let mut targets = vec![target];
    match target {
        "wasm32-wasip1" => targets.push("wasm32-wasi"),
        "wasm32-wasip1-threads" => targets.push("wasm32-wasi-threads"),
        _ => {}
    }
    targets
}

/// The directory containing WASI's libc and `libsetjmp` for `target`.
/// Older SDKs used the pre-preview-name target directories, which contain
/// the same ABI libraries.
pub fn library_dir(sysroot: &Path, target: &str) -> Option<PathBuf> {
    library_targets(target)
        .into_iter()
        .map(|library_target| sysroot.join("lib").join(library_target))
        .find(|lib| lib.join("libc.a").is_file() && lib.join("libsetjmp.a").is_file())
}

/// A WASI sysroot with the libc and `libsetjmp` of `target`.
pub fn sysroot(target: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(sysroot) = env::var_os("WASI_SYSROOT") {
        push_unique(&mut candidates, PathBuf::from(sysroot));
    }
    for variable in SDK_ENV {
        if let Some(sdk) = env::var_os(variable) {
            sdk_sysroot_candidates(&mut candidates, PathBuf::from(sdk));
        }
    }
    for variable in CLANG_ENV {
        if let Some(compiler) = env::var_os(variable) {
            compiler_sysroot_candidates(&mut candidates, Path::new(&compiler));
        }
    }
    for name in ["clang", "clang-21", "clang-20", "clang-19", "clang-18"] {
        if let Some(compiler) = program_in_path(name) {
            compiler_sysroot_candidates(&mut candidates, &compiler);
        }
    }
    if let Some(prefix) = package_prefix("wasi-libc") {
        sdk_sysroot_candidates(&mut candidates, prefix);
    }

    candidates
        .into_iter()
        .find(|candidate| library_dir(candidate, target).is_some())
}

fn sdk_clang(sdk: &Path) -> Option<PathBuf> {
    let bin = sdk.join("bin");
    [bin.join("clang"), bin.join("clang.exe")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn clang_accepts_wasm_sjlj(clang: &Path, target: &str) -> bool {
    let probe = env::temp_dir().join(format!(
        "caribou-clang-probe-{}-{}.o",
        std::process::id(),
        target
    ));
    let status = Command::new(clang)
        .arg(format!("--target={target}"))
        .args([
            "-mexception-handling",
            "-mllvm",
            "-wasm-enable-sjlj",
            "-mllvm",
            "-wasm-use-legacy-eh=false",
            "-x",
            "c",
            "-c",
            "-",
            "-o",
        ])
        .arg(&probe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let _ = std::fs::remove_file(probe);
    status
}

/// A clang whose WebAssembly backend accepts the setjmp lowering flags.
/// Discovery is based on configuration and compiler capability rather than
/// host operating-system paths.
pub fn clang(target: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for variable in CLANG_ENV {
        if let Some(compiler) = env::var_os(variable) {
            push_unique(&mut candidates, PathBuf::from(compiler));
        }
    }
    for variable in SDK_ENV {
        if let Some(sdk) = env::var_os(variable) {
            let sdk = PathBuf::from(sdk);
            if let Some(compiler) = sdk_clang(&sdk) {
                push_unique(&mut candidates, compiler);
            }
            if let Some(parent) = sdk.parent().and_then(sdk_clang) {
                push_unique(&mut candidates, parent);
            }
        }
    }
    for name in ["clang", "clang-21", "clang-20", "clang-19", "clang-18"] {
        if let Some(compiler) = program_in_path(name) {
            push_unique(&mut candidates, compiler);
        }
    }
    if let Some(prefix) = package_prefix("llvm")
        && let Some(compiler) = sdk_clang(&prefix)
    {
        push_unique(&mut candidates, compiler);
    }
    candidates
        .into_iter()
        .find(|candidate| clang_accepts_wasm_sjlj(candidate, target))
}

fn target_env_is_set(prefix: &str, target: &str) -> bool {
    env::var_os(format!("{prefix}_{target}")).is_some()
        || env::var_os(format!("{prefix}_{}", target.replace('-', "_"))).is_some()
}

/// Whether Cargo/cc has already been given a target compiler.
pub fn compiler_is_configured(target: &str) -> bool {
    target_env_is_set("CC", target) || env::var_os("TARGET_CC").is_some()
}

/// Whether Cargo/cc has already been given target-specific C flags.
pub fn cflags_are_configured(target: &str) -> bool {
    target_env_is_set("CFLAGS", target) || env::var_os("TARGET_CFLAGS").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(directory: &Path, name: &str) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join(name), []).unwrap();
    }

    #[test]
    fn library_dir_accepts_preview_one_and_legacy_names() {
        let root = env::temp_dir().join(format!("caribou-wasi-test-{}", std::process::id()));
        let legacy = root.join("lib/wasm32-wasi");
        archive(&legacy, "libc.a");
        archive(&legacy, "libsetjmp.a");

        assert_eq!(library_dir(&root, "wasm32-wasip1"), Some(legacy));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn library_dir_requires_both_archives() {
        let root = env::temp_dir().join(format!(
            "caribou-wasi-incomplete-test-{}",
            std::process::id()
        ));
        archive(&root.join("lib/wasm32-wasip1"), "libc.a");

        assert_eq!(library_dir(&root, "wasm32-wasip1"), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
