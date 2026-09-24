#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // the whole numbering, not only what is used yet
#[repr(i32)]
pub enum Kind {
    Instance = 1,
    Adapter = 2,
    Device = 3,
    Queue = 4,
    Buffer = 5,
    Texture = 6,
    View = 7,
    Sampler = 8,
    Shader = 9,
    Bindgroup = 10,
    Pipeline = 11,
    Renderpipeline = 12,
    Encoder = 13,
    Surface = 14,
    Builder = 15,
    Bindings = 16,
    BindGroupLayout = 17,
    PipelineLayout = 18,
}
