//! Generated object bindings over the native GPU backend. See gpu.api.rs.
#![allow(non_snake_case, clippy::too_many_arguments)]
#![recursion_limit = "512"]

#[cfg(not(feature = "native"))]
compile_error!(
    "caribou-gpu requires its wgpu backend; disable default features only for schema generation"
);

mod backend;
mod handles;
mod types;
use caribou_abi::{Buffer, Enum, Future, Text};
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
            unsafe {
                __CARIBOU_CLASSES[request.future_ret_class as usize]
                    .name
                    .as_str()
            },
            "GpuAdapter"
        );
        assert_eq!(request.ret, caribou_abi::TypeTag::FUTURE);
        assert_eq!(request.future_ret, caribou_abi::TypeTag::OBJ);
        let device = symbol("GpuAdapter", "requestDevice");
        assert_eq!(device.ret, caribou_abi::TypeTag::FUTURE);
        assert_eq!(device.future_ret, caribou_abi::TypeTag::OBJ);
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[device.future_ret_class as usize]
                    .name
                    .as_str()
            },
            "GpuDevice"
        );
        assert_eq!(BlendFactor::OneMinusSrcAlpha.native(), 5);
        assert_eq!(AddressMode::MirrorRepeat.native(), 2);
        assert_eq!(BufferUsage::STORAGE(), 128);
        assert_eq!(TextureUsage::RENDER_ATTACHMENT(), 16);
        assert_eq!(Feature::ShaderF16.native(), 10);
        assert_eq!(Feature::Subgroups.native(), 17);
        assert_eq!(Limit::MaxBufferSize.native(), 24);
        assert_eq!(VertexFormat::Float32x2.native(), 28);
        assert_eq!(VertexFormat::Unorm1010102.native(), 39);
        assert_eq!(VertexFormat::Unorm8x4Bgra.native(), 40);
        assert_eq!(TextureFormat::Rgba8unorm.native(), 21);
        assert_eq!(TextureFormat::Depth32floatStencil8.native(), 48);
        assert_eq!(TextureFormat::Bc7RgbaUnorm.native(), 61);
        assert_eq!(TextureFormat::Astc12x12UnormSrgb.native(), 100);

        let configured = symbol("GpuAdapter", "requestDeviceWith");
        assert_eq!(configured.ret, caribou_abi::TypeTag::FUTURE);
        assert_eq!(configured.future_ret, caribou_abi::TypeTag::OBJ);
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[configured.param_classes[1] as usize]
                    .name
                    .as_str()
            },
            "GpuDeviceDescriptor"
        );

        let map = symbol("GpuDevice", "mapBuffer");
        assert_eq!(map.ret, <Future<()> as caribou_abi::Returned>::TAG);
        assert_eq!(map.future_ret, caribou_abi::TypeTag::VOID);
        let submitted = symbol("GpuDevice", "queueWorkDone");
        assert_eq!(submitted.ret, <Future<()> as caribou_abi::Returned>::TAG);
        assert_eq!(submitted.future_ret, caribou_abi::TypeTag::VOID);
        assert!(
            __CARIBOU_CLASSES
                .iter()
                .all(|class| unsafe { class.name.as_str() } != "GpuRequest")
        );

        let create_buffer = symbol("GpuDevice", "createBuffer");
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[create_buffer.param_classes[1] as usize]
                    .name
                    .as_str()
            },
            "GpuBufferDescriptor"
        );
        let mut descriptor = GpuBufferDescriptor::new(64, BufferUsage::STORAGE());
        assert_eq!(descriptor.size, 64);
        assert_eq!(descriptor.mappedAtCreation, None);
        GpuBufferDescriptor::mappedAtCreation(&mut descriptor, true);
        assert_eq!(descriptor.mappedAtCreation, Some(true));
        let mut requested = GpuDeviceDescriptor::new();
        requested.requiredFeatures.push(Feature::ShaderF16.native());
        requested
            .requiredLimits
            .push((Limit::MaxBindGroups.native(), 8));
        assert_eq!(requested.requiredFeatures, [Feature::ShaderF16.native()]);
        assert_eq!(
            requested.requiredLimits,
            [(Limit::MaxBindGroups.native(), 8)]
        );

        let sampler = symbol("GpuDevice", "sampler");
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[sampler.param_classes[1] as usize]
                    .name
                    .as_str()
            },
            "GpuSamplerDescriptor"
        );
        assert!(!symbol("GpuSamplerDescriptor", "addressModeU").param_enums[1].is_null());
        assert!(!symbol("GpuSamplerDescriptor", "mipmapFilter").param_enums[1].is_null());

        let view = symbol("GpuTexture", "createView");
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[view.param_classes[1] as usize]
                    .name
                    .as_str()
            },
            "GpuTextureViewDescriptor"
        );
        assert!(!symbol("GpuTextureViewDescriptor", "dimension").param_enums[1].is_null());

        let texture = symbol("GpuDevice", "texture");
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[texture.param_classes[1] as usize]
                    .name
                    .as_str()
            },
            "GpuTextureDescriptor"
        );
        let texture_descriptor = symbol("GpuTextureDescriptor", "new");
        assert_eq!(
            unsafe {
                __CARIBOU_CLASSES[texture_descriptor.param_classes[0] as usize]
                    .name
                    .as_str()
            },
            "GpuExtent3D"
        );
        assert!(!texture_descriptor.param_enums[1].is_null());
        assert!(!symbol("GpuTextureDescriptor", "addViewFormats").param_enums[1].is_null());
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
