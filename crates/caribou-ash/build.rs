use std::env;

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
    for variable in caribou_wasm_toolchain::CONFIG_ENV {
        println!("cargo:rerun-if-env-changed={variable}");
    }
}

/// On wasm the jump is LLVM's setjmp lowering over the exception-handling
/// proposal, as Ash compiles its own code: both halves, and not the legacy
/// `try`/`catch` that no current engine accepts. It needs a clang with the
/// WebAssembly backend and a WASI sysroot. The shared toolchain discovery
/// honours explicit configuration and SDKs on PATH; cc's target variables
/// still take precedence over a discovered compiler.
fn wasm(build: &mut cc::Build, target: &str) {
    if !caribou_wasm_toolchain::compiler_is_configured(target) {
        if let Some(clang) = caribou_wasm_toolchain::clang(target) {
            build.compiler(clang);
        }
    }
    if let Some(sysroot) = caribou_wasm_toolchain::sysroot(target) {
        build.flag(format!("--sysroot={}", sysroot.display()));
    }
    build
        .flag("-mexception-handling")
        .flag("-mllvm")
        .flag("-wasm-enable-sjlj")
        .flag("-mllvm")
        .flag("-wasm-use-legacy-eh=false");
}
