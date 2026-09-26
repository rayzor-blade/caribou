use std::env;
use std::path::{Path, PathBuf};

fn main() {
    // The setjmp frame under every call into HashLink code; see trap.c.
    let mut build = cc::Build::new();
    build.file("src/trap.c").warnings(true);
    let target = env::var("TARGET").unwrap_or_default();
    if target.starts_with("wasm32") {
        wasm(&mut build, &target);
    }
    build.compile("caribou_ash_trap");
    println!("cargo:rerun-if-changed=src/trap.c");
    println!("cargo:rerun-if-env-changed=WASI_SYSROOT");
}

/// On wasm the jump is LLVM's setjmp lowering over the exception-handling
/// proposal, as Ash compiles its own code: both halves, and not the legacy
/// `try`/`catch` that no current engine accepts. It needs a clang with the
/// WebAssembly backend and a WASI sysroot, found where Ash's runtime build
/// looks for them unless cc's target variables name them; a plain `CC` is
/// the host's compiler.
fn wasm(build: &mut cc::Build, target: &str) {
    let cc_var = format!("CC_{}", target.replace('-', "_"));
    if env::var_os(&cc_var).is_none() && env::var_os("TARGET_CC").is_none() {
        let clang = [
            "/opt/homebrew/opt/llvm/bin/clang",
            "/usr/local/opt/llvm/bin/clang",
            "/opt/wasi-sdk/bin/clang",
            "/usr/local/wasi-sdk/bin/clang",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file());
        if let Some(clang) = clang {
            build.compiler(clang);
        }
    }
    if env::var_os("WASI_SYSROOT").is_none() {
        let sysroot = [
            "/opt/homebrew/opt/wasi-libc/share/wasi-sysroot",
            "/usr/local/opt/wasi-libc/share/wasi-sysroot",
            "/opt/wasi-sdk/share/wasi-sysroot",
            "/usr/local/wasi-sdk/share/wasi-sysroot",
            "/usr/share/wasi-sysroot",
        ]
        .into_iter()
        .map(Path::new)
        .find(|p| p.join("lib").join(target).join("libc.a").is_file());
        if let Some(sysroot) = sysroot {
            build.flag(format!("--sysroot={}", sysroot.display()));
        }
    }
    build
        .flag("-mexception-handling")
        .flag("-mllvm")
        .flag("-wasm-enable-sjlj")
        .flag("-mllvm")
        .flag("-wasm-use-legacy-eh=false");
}
