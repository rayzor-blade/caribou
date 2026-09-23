fn main() {
    println!("cargo:rerun-if-changed=gpu.api.rs");
    println!("cargo:rerun-if-changed=spec/webgpu.idl");
    let api = std::fs::read_to_string("gpu.api.rs").unwrap();
    let idl = std::fs::read_to_string("spec/webgpu.idl").unwrap();
    let generated =
        caribou_bindgen::generate("gpu", &api, &idl).expect("valid GPU binding declarations");
    let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(path.join("gpu.rs"), generated).unwrap();
}
