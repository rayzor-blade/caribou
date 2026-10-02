//! The window plugin's model, generated from xwindow's declaration with one
//! class of probes appended, over xwindow's own native backend.

use std::path::PathBuf;

/// Events as a window reports them, made without opening one.
const SAMPLES: &str = r#"
trait Samples {
    #[native(samples_event)]
    fn event(which: i32) -> Event;
    #[native(samples_sizing)]
    fn sizing(which: i32) -> Enum<ScaleSizing>;
}
"#;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    xwindow_backend::install(&out).expect("xwindow's backend installs");
    let declaration = out.join("window.api.rs");
    std::fs::write(
        &declaration,
        format!("{}\n{SAMPLES}", xwindow_bindgen::window_api()),
    )
    .unwrap();
    let model = x_idl::generate_caribou(
        xwindow_bindgen::NAMESPACE,
        Some(declaration),
        &xwindow_bindgen::browser_idl(),
    )
    .expect("the window model generates");
    std::fs::write(out.join("window.rs"), model).unwrap();
}
