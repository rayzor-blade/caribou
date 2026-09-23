//! Generated object bindings over the native GPU backend. See gpu.api.rs.
#![allow(non_snake_case, clippy::too_many_arguments)]
#![recursion_limit = "512"]

#[cfg(not(feature = "native"))]
compile_error!(
    "caribou-gpu currently requires its native backend; no wasm forwarding backend is implemented"
);

mod backend;
mod handles;
mod types;
use caribou_abi::{Buffer, Enum, Text};
include!(concat!(env!("OUT_DIR"), "/gpu.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_catalog_preserves_classes_buffers_text_and_enums() {
        assert_eq!(unsafe { __CARIBOU_INFO.name.as_str() }, "gpu");
        let symbol = |class: &str, name: &str| {
            __CARIBOU_SYMBOLS
                .iter()
                .find(|s| unsafe { s.class.as_str() == class && s.method.as_str() == name })
                .unwrap()
        };
        let shader = symbol("GpuDevice", "createShader");
        assert_eq!(shader.params[1], <Text as caribou_abi::Param>::TAG);
        assert_eq!(
            unsafe { __CARIBOU_CLASSES[shader.ret_class as usize].name.as_str() },
            "GpuShader"
        );
        let write = symbol("GpuQueue", "writeBuffer");
        assert_eq!(write.params[3], <Buffer as caribou_abi::Param>::TAG);
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[write.param_classes[1] as usize]
                    .name
                    .as_str()
            },
            "GpuBuffer"
        );
        let request = symbol("GpuInstance", "requestAdapter");
        assert!(!request.param_enums[1].is_null());
        assert_eq!(
            unsafe { __CARIBOU_CLASSES[request.ret_class as usize].name.as_str() },
            "GpuAdapter"
        );
        assert_eq!(BlendFactor::OneMinusSrcAlpha.native(), 5);
        assert_eq!(AddressMode::MirrorRepeat.native(), 2);
        assert_eq!(BufferUsage::STORAGE(), 128);
        assert_eq!(TextureUsage::RENDER_ATTACHMENT(), 16);
    }
    #[test]
    fn generated_resource_methods_preserve_native_identity() {
        let bindings = GpuBindings::new();
        assert!(GpuBindings::valid(&bindings));
        GpuBindings::destroy(&bindings);
        assert!(!GpuBindings::valid(&bindings));
        GpuBindings::destroy(&bindings);
        let newer = GpuBindings::new();
        assert!(GpuBindings::valid(&newer));
        assert!(!GpuBindings::valid(&bindings));
        GpuBindings::destroy(&newer);
    }
}
