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
    // The wire to a browser's WebGPU: the plugin's encoder, and the GPU
    // agent's decoder, which a web build carries for the program to ship.
    let wire = caribou_bindgen::wire::wire(&idl).expect("the WebGPU IDL generates its wire");
    std::fs::write(path.join("gpu_wire.rs"), wire.rust).unwrap();
    // A page starts the module as the GPU agent.
    let agent = wire.js + AGENT_START;
    std::fs::write(path.join("gpu_agent.mjs"), agent).unwrap();
    // For a WASI program, the backend the members call: what src/web.rs
    // defines, and a refusal for the rest.
    println!("cargo:rerun-if-changed=src/web.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        let web = std::fs::read_to_string("src/web.rs").unwrap();
        let backend = caribou_bindgen::web_backend("gpu", &api, &idl, &web)
            .expect("the web backend generates");
        std::fs::write(path.join("gpu_web_backend.rs"), backend).unwrap();
    }
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

/// The GPU agent's start, after the generated wire: the browser's GPU under
/// handle 1 and, when the page gives it one, the canvas's WebGPU context
/// under handle 2, the program's surface. A batch that drew on the canvas
/// counts as run once the frame reached the page, which paces a program
/// that draws to the display.
const AGENT_START: &str = r#"
function canvas() {
  return new Promise((resolve) => {
    const answer = ({ data }) => {
      if (data.canvas === undefined) return;
      self.removeEventListener("message", answer);
      resolve(data.canvas);
    };
    self.addEventListener("message", answer);
    self.postMessage({ kind: "canvas" });
  });
}

if (typeof WorkerGlobalScope !== "undefined") {
  start(
    async () => {
      const roots = new Map([[1, navigator.gpu]]);
      const surface = await canvas();
      if (surface) roots.set(2, surface.getContext("webgpu"));
      return roots;
    },
    (wire) => {
      const context = wire.objects.get(2);
      if (!context) return;
      let drew = false;
      const current = context.getCurrentTexture.bind(context);
      context.getCurrentTexture = () => {
        drew = true;
        return current();
      };
      wire.after = () => {
        if (!drew) return;
        drew = false;
        return new Promise((resolve) =>
          typeof requestAnimationFrame === "function"
            ? requestAnimationFrame(resolve)
            : setTimeout(resolve, 16),
        );
      };
    },
  );
}
"#;
