//! An agent module, as a plugin whose page-side half runs beside the
//! program ships one: the driver writes it beside a wasm build.
fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("math_agent.mjs"), "// The math plugin's agent.\n").unwrap();
}
