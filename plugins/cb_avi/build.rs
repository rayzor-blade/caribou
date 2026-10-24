//! xavi's `media` model for Caribou, its native and page backends, and
//! what a page serves for the plugin: the agent and the wire it imports.

use std::path::PathBuf;

use xavi_bindgen::Runtime;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    xavi_backend::install(&out).expect("xavi's backend installs");
    let model = xavi_bindgen::generate(Runtime::Caribou).expect("xavi's model generates");
    std::fs::write(out.join("media.rs"), model).unwrap();
    // The page's half of the wire beside the agent `install` wrote, which
    // a program in a page ships with its other page files.
    let wire = xavi_bindgen::browser_wire().expect("xavi's wire generates");
    std::fs::write(out.join("page").join(xavi_backend::WIRE_MODULE), &wire.js).unwrap();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        std::fs::write(out.join("media_wire.rs"), &wire.rust).unwrap();
        let backend = xavi_bindgen::web_backend(Runtime::Caribou, xavi_backend::WEB)
            .expect("xavi's web backend generates");
        std::fs::write(out.join("media_web_backend.rs"), backend).unwrap();
    }
}
