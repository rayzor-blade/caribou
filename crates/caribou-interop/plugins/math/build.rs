//! A page part, as a plugin that has one ships it: its files in
//! `OUT_DIR/page`, which the driver writes beside a wasm build.
fn main() {
    let page = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("page");
    std::fs::create_dir_all(&page).unwrap();
    std::fs::write(page.join("math.mjs"), "export function start() {}\n").unwrap();
}
