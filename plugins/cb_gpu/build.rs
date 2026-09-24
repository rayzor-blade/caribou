fn main() {
    println!("cargo:rerun-if-changed=gpu.api.rs");
    println!("cargo:rerun-if-changed=spec/webgpu.idl");
    let mut api = std::fs::read_to_string("gpu.api.rs").unwrap();
    api.push_str(&native_features());
    let idl = std::fs::read_to_string("spec/webgpu.idl").unwrap();
    let generated =
        caribou_bindgen::generate("gpu", &api, &idl).expect("valid GPU binding declarations");
    let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(path.join("gpu.rs"), generated).unwrap();
}

/// `enum NativeFeature`: every feature wgpu has, WebGPU's and its own, in
/// `wgpu::Features::all()` order, which is the order the backend maps back.
fn native_features() -> String {
    let variants: Vec<String> = wgpu_types::Features::all()
        .iter_names()
        .map(|(name, _)| {
            name.split('_')
                .map(|word| {
                    let lower = word.to_ascii_lowercase();
                    let mut chars = lower.chars();
                    chars.next().map_or(String::new(), |first| {
                        first.to_ascii_uppercase().to_string() + chars.as_str()
                    })
                })
                .collect()
        })
        .collect();
    format!("\nenum NativeFeature {{ {} }}\n", variants.join(", "))
}
