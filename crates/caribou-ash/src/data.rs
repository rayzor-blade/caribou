//! Native Haxe views of the core's data values. Buffer bytes are shared;
//! enum payload slots are translated using the loaded program's layout.
use crate::proto;
#[cfg(feature = "runner")]
use caribou::registry::TypeRef;
use caribou::{
    data,
    heap::{self, Handle, TypeDesc},
};
use caribou_abi::{
    Value,
    data::{BufferData, EnumData},
    hl::{self, hl_type, vdynamic},
};
use std::{
    collections::HashMap,
    sync::{
        LazyLock, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy)]
struct BytesType {
    t: usize,
    length: usize,
    bytes: usize,
}
#[derive(Default)]
struct Types {
    bytes: Option<BytesType>,
    enums: HashMap<usize, &'static TypeDesc>,
    by_name: HashMap<String, usize>,
}
static BYTES_TYPE: AtomicUsize = AtomicUsize::new(0);
static TYPES: LazyLock<RwLock<Types>> = LazyLock::new(|| RwLock::new(Types::default()));

#[cfg(feature = "runner")]
unsafe fn name(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// Reject a stale enum declaration before any payload can be read or written.
#[cfg(feature = "runner")]
pub(crate) fn attach(types: impl Iterator<Item = *mut hl_type>) -> anyhow::Result<()> {
    let mut found = Types::default();
    for t in types {
        if t.is_null() {
            continue;
        }
        unsafe {
            if proto::obj_name_is(t, "haxe.io.Bytes") {
                let obj = &*(*t).detail.obj;
                let rt = ash_std::obj::hlp_get_obj_rt(t.cast());
                let field = |want: &str| {
                    (0..obj.nfields as usize)
                        .find(|&i| name((*obj.fields.add(i)).name) == want)
                        .map(|i| *(*rt).fields_indexes.add(i) as usize)
                };
                found.bytes = Some(BytesType {
                    t: t as usize,
                    length: field("length")
                        .ok_or_else(|| anyhow::anyhow!("Bytes.length missing"))?,
                    bytes: field("b").ok_or_else(|| anyhow::anyhow!("Bytes.b missing"))?,
                });
            } else if (*t).kind == hl::HENUM {
                let e = &*(*t).detail.tenum;
                let enum_name = name(e.name);
                let Some(desc) = data::enum_type(&enum_name) else {
                    continue;
                };
                let schema = data::enum_schema(desc);
                anyhow::ensure!(
                    e.nconstructs as usize == schema.variants.len(),
                    "enum {enum_name} constructors changed; rebuild Haxe bytecode"
                );
                for (i, variant) in schema.variants.iter().enumerate() {
                    let c = &*e.constructs.add(i);
                    anyhow::ensure!(
                        name(c.name) == variant.name && c.nparams as usize == variant.fields.len(),
                        "enum {enum_name} constructor changed; rebuild Haxe bytecode"
                    );
                    for (j, field) in variant.fields.iter().enumerate() {
                        anyhow::ensure!(
                            matches_type(*c.params.add(j), &field.ty),
                            "enum {enum_name} field {} changed; rebuild Haxe bytecode",
                            field.name
                        );
                    }
                }
                found.enums.insert(t as usize, desc);
                found.by_name.insert(enum_name, t as usize);
            }
        }
    }
    BYTES_TYPE.store(found.bytes.map_or(0, |b| b.t), Ordering::Release);
    *TYPES.write().unwrap() = found;
    Ok(())
}
#[cfg(feature = "runner")]
unsafe fn matches_type(t: *const hl_type, ty: &TypeRef) -> bool {
    let kind = unsafe { (*t).kind };
    match ty {
        TypeRef::Int => matches!(kind, hl::HUI8 | hl::HUI16 | hl::HI32),
        TypeRef::Int64 => kind == hl::HI64,
        TypeRef::Float => matches!(kind, hl::HF32 | hl::HF64),
        TypeRef::Bool => kind == hl::HBOOL,
        TypeRef::Str => unsafe { proto::obj_name_is(t, "String") },
        TypeRef::Buffer => unsafe { proto::obj_name_is(t, "haxe.io.Bytes") },
        TypeRef::Future(_) => unsafe { proto::obj_name_is(t, "caribou.Future") },
        TypeRef::Enum(n) => kind == hl::HENUM && unsafe { name((*(*t).detail.tenum).name) == *n },
        TypeRef::Dyn => kind == hl::HDYN,
        _ => false,
    }
}
pub(crate) fn is_buffer(t: *const hl_type) -> bool {
    t as usize == BYTES_TYPE.load(Ordering::Acquire)
}
pub(crate) fn is_enum(t: *const hl_type) -> bool {
    TYPES.read().unwrap().enums.contains_key(&(t as usize))
}

struct Root(Handle);
impl Root {
    fn pointer(p: *mut u8) -> Self {
        Self(heap::handle_new(p))
    }
    fn value(v: Value) -> Self {
        Self(
            v.as_object()
                .map_or(Handle::NULL, |p| heap::handle_new(p.cast())),
        )
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        heap::handle_release(self.0);
    }
}

pub(crate) unsafe fn buffer_from_haxe(p: *mut vdynamic) -> Value {
    let shape = TYPES.read().unwrap().bytes.unwrap();
    let _root = Root::pointer(p.cast());
    let base = p.cast::<u8>();
    let len = unsafe { *base.add(shape.length).cast::<i32>() };
    let bytes = unsafe { *base.add(shape.bytes).cast::<*mut u8>() };
    if len < 0 || (len > 0 && bytes.is_null()) {
        return Value::null();
    }
    Value::object(unsafe { data::buffer_share(bytes, len as usize) }.cast())
}
pub(crate) unsafe fn buffer_to_haxe(p: *mut BufferData) -> Result<*mut vdynamic, String> {
    let shape = TYPES
        .read()
        .unwrap()
        .bytes
        .ok_or("the program has no haxe.io.Bytes type")?;
    let _root = Root::pointer(p.cast());
    let len = i32::try_from(unsafe { (*p).len }).map_err(|_| "buffer exceeds Haxe Bytes length")?;
    let obj = unsafe { ash_std::obj::hlp_alloc_obj((shape.t as *mut hl_type).cast()) }.cast::<u8>();
    unsafe {
        *obj.add(shape.length).cast::<i32>() = len;
        *obj.add(shape.bytes).cast::<*mut u8>() = (*p).bytes;
    }
    Ok(obj.cast())
}
pub(crate) unsafe fn enum_from_haxe(p: *mut vdynamic) -> Value {
    let _root = Root::pointer(p.cast());
    let e = p.cast::<hl::venum>();
    let t = unsafe { (*e).t };
    let desc = TYPES.read().unwrap().enums[&(t as usize)];
    let index = unsafe { (*e).index };
    let schema = data::enum_schema(desc);
    if index < 0 || index as usize >= schema.variants.len() {
        return Value::null();
    }
    let c = unsafe { &*(*(*t).detail.tenum).constructs.add(index as usize) };
    let mut fields = Vec::with_capacity(c.nparams as usize);
    let mut roots = Vec::with_capacity(c.nparams as usize);
    for i in 0..c.nparams as usize {
        let value = unsafe {
            proto::read_kind(
                p.cast::<u8>().add(*c.offsets.add(i) as usize),
                (**c.params.add(i)).kind,
            )
        }
        .unwrap_or(Value::null());
        roots.push(Root::value(value));
        fields.push(value);
    }
    match data::enum_new(desc, index as u32, &fields) {
        Ok(p) => Value::object(p.cast()),
        Err(message) => {
            caribou::bridge::set_pending(caribou::error::Error::value(caribou::error::Error::new(
                caribou_abi::ErrorKind::Type,
                &message,
                proto::lang(),
            )));
            Value::null()
        }
    }
}
pub(crate) unsafe fn enum_to_haxe(p: *mut EnumData) -> Result<*mut vdynamic, String> {
    let _root = Root::pointer(p.cast());
    let schema = unsafe { data::enum_schema(data::enum_descriptor(p)) };
    let t = *TYPES
        .read()
        .unwrap()
        .by_name
        .get(&schema.name)
        .ok_or_else(|| format!("the program has no enum {}", schema.name))?
        as *mut hl_type;
    let index = unsafe { (*p).index };
    let c = unsafe { &*(*(*t).detail.tenum).constructs.add(index as usize) };
    let obj = unsafe { ash_std::types::hlp_alloc_enum(t.cast(), index as i32) }.cast::<u8>();
    if obj.is_null() {
        return Err("could not allocate Haxe enum".into());
    }
    let _obj_root = Root::pointer(obj);
    for (i, value) in unsafe { data::enum_fields(p) }.iter().enumerate() {
        unsafe {
            proto::write_kind(
                obj.add(*c.offsets.add(i) as usize),
                (**c.params.add(i)).kind,
                *value,
            )
        }
        .ok_or("invalid enum payload type")??;
    }
    Ok(obj.cast())
}
