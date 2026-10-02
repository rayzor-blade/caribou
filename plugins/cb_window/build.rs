//! xwindow's `window` model for Caribou, its winit and page backends, and
//! what a page serves for the plugin: the agent and the wire it imports.

use std::path::PathBuf;

use xwindow_bindgen::Runtime;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    xwindow_backend::install(&out).expect("xwindow's backend installs");
    let model = xwindow_bindgen::generate(Runtime::Caribou).expect("xwindow's model generates");
    std::fs::write(out.join("window.rs"), model).unwrap();
    // The page's half of the wire beside the agent `install` wrote, which
    // a program in a page ships with its other page files.
    let wire = xwindow_bindgen::browser_wire().expect("xwindow's wire generates");
    std::fs::write(
        out.join("page").join(xwindow_backend::WIRE_MODULE),
        &wire.js,
    )
    .unwrap();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        std::fs::write(out.join("window_wire.rs"), &wire.rust).unwrap();
        let backend = xwindow_bindgen::web_backend(Runtime::Caribou, xwindow_backend::WEB)
            .expect("xwindow's web backend generates");
        std::fs::write(out.join("window_web_backend.rs"), backend).unwrap();
    }
}
