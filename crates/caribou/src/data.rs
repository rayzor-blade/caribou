//! Heap-owned binary storage and algebraic enum values shared by adapters.
use crate::{
    describe,
    error::{Rooted, Str},
    heap::{self, Tracer, TypeDesc},
    protocol::{self, Protocol, REPLY_MISSING, REPLY_OK},
    registry::TypeRef,
};
use caribou_abi::{
    TypeTag, Value,
    data::{BufferData, EnumData},
};
use std::{
    collections::HashMap,
    ptr,
    sync::{LazyLock, RwLock},
};

const fn descriptor(
    name: &'static str,
    trace: heap::TraceFn,
    protocol: &'static Protocol,
) -> TypeDesc {
    let mut d = TypeDesc::new(crate::error::core_type());
    d.name = name.as_ptr();
    d.name_len = name.len();
    d.trace = Some(trace);
    d.protocol = protocol;
    d
}

pub static BUFFER_DESC: TypeDesc = descriptor("caribou.Buffer", trace_buffer, &BUFFER_PROTO);

pub fn buffer_new(bytes: &[u8]) -> *mut BufferData {
    let root = Rooted::alloc(&BUFFER_DESC, size_of::<BufferData>());
    let p = root.ptr().cast::<BufferData>();
    let data = unsafe {
        heap::alloc_gen(
            ptr::null_mut(),
            bytes.len().max(1),
            caribou_abi::mem::KIND_NOPTR,
        )
    }
    .cast::<u8>();
    if data.is_null() {
        heap::out_of_memory("buffer bytes");
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        (*p).len = bytes.len();
        (*p).bytes = data;
    }
    p
}

/// Share a live GC allocation; the buffer's trace retains the backing block.
/// The caller must keep `bytes` rooted through this allocation.
pub unsafe fn buffer_share(bytes: *mut u8, len: usize) -> *mut BufferData {
    let root = Rooted::alloc(&BUFFER_DESC, size_of::<BufferData>());
    let p = root.ptr().cast::<BufferData>();
    unsafe {
        (*p).len = len;
        (*p).bytes = bytes;
    }
    p
}

pub fn buffer_of(v: Value) -> Option<*mut BufferData> {
    let p = crate::cell::unwrap(v).as_object()?.cast::<u8>();
    (!p.is_null() && ptr::eq(unsafe { protocol::desc_of(p) }, &BUFFER_DESC)).then_some(p.cast())
}

unsafe extern "C" fn trace_buffer(p: *mut u8, tracer: *mut Tracer) {
    unsafe {
        (*tracer).mark((*p.cast::<BufferData>()).bytes);
    }
}
unsafe extern "C-unwind" fn buffer_len(p: *mut u8, out: *mut usize) -> u8 {
    unsafe {
        *out = (*p.cast::<BufferData>()).len;
    }
    REPLY_OK
}
fn index(v: Value) -> Option<usize> {
    v.as_int()
        .and_then(|n| usize::try_from(n).ok())
        .or_else(|| {
            v.as_number()
                .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
                .map(|n| n as usize)
        })
}
unsafe extern "C-unwind" fn buffer_index(p: *mut u8, key: Value, out: *mut Value) -> u8 {
    let b = unsafe { &*p.cast::<BufferData>() };
    let Some(i) = index(key).filter(|i| *i < b.len) else {
        return REPLY_MISSING;
    };
    unsafe {
        *out = Value::int(*b.bytes.add(i) as i32);
    }
    REPLY_OK
}
unsafe extern "C-unwind" fn buffer_set(p: *mut u8, key: Value, v: Value) -> u8 {
    let b = unsafe { &*p.cast::<BufferData>() };
    let Some(i) = index(key).filter(|i| *i < b.len) else {
        return REPLY_MISSING;
    };
    let Some(n) = index(v).filter(|n| *n <= 255) else {
        return REPLY_MISSING;
    };
    unsafe {
        *b.bytes.add(i) = n as u8;
    }
    REPLY_OK
}
unsafe extern "C-unwind" fn buffer_iterate(p: *mut u8, state: *mut Value, out: *mut Value) -> u8 {
    let Some(i) = (unsafe { index(*state).map_or(Some(0), |i| i.checked_add(1)) }) else {
        return REPLY_MISSING;
    };
    let reply = unsafe { buffer_index(p, Value::number(i as f64), out) };
    if reply == REPLY_OK {
        unsafe {
            *state = Value::number(i as f64);
        }
    }
    reply
}
static BUFFER_PROTO: Protocol = Protocol {
    len: Some(buffer_len),
    index: Some(buffer_index),
    set_index: Some(buffer_set),
    iterate: Some(buffer_iterate),
    ..Protocol::NONE
};

struct EnumType {
    schema: describe::EnumDesc,
    name: crate::symbol::Symbol,
}

static ENUMS: LazyLock<RwLock<HashMap<String, &'static TypeDesc>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a declaration once per fully qualified name. No per-value metadata.
pub fn register_enum(
    schema: describe::EnumDesc,
    lang: caribou_abi::LangId,
) -> Result<&'static TypeDesc, String> {
    let mut enums = ENUMS.write().unwrap();
    if let Some(d) = enums.get(&schema.name) {
        return if enum_schema(d) == &schema {
            Ok(d)
        } else {
            Err(format!("conflicting enum declaration {}", schema.name))
        };
    }
    let name = String::leak(schema.name.clone());
    let mut d = descriptor(name, trace_enum, &ENUM_PROTO);
    d.lang = lang;
    d.ext = Box::into_raw(Box::new(EnumType {
        name: crate::symbol::intern(name),
        schema,
    }))
    .cast();
    let d = Box::leak(Box::new(d));
    enums.insert(name.to_owned(), d);
    Ok(d)
}
pub fn enum_type(name: &str) -> Option<&'static TypeDesc> {
    ENUMS.read().unwrap().get(name).copied()
}
pub fn is_enum(d: &TypeDesc) -> bool {
    ptr::eq(d.protocol, &ENUM_PROTO)
}
pub fn enum_schema(d: &TypeDesc) -> &describe::EnumDesc {
    assert!(is_enum(d));
    unsafe { &(*d.ext.cast::<EnumType>()).schema }
}
pub fn enum_of(v: Value) -> Option<*mut EnumData> {
    let p = crate::cell::unwrap(v).as_object()?.cast::<u8>();
    if p.is_null() || !is_enum(unsafe { &*protocol::desc_of(p) }) {
        return None;
    }
    Some(p.cast())
}
pub unsafe fn enum_fields<'a>(p: *const EnumData) -> &'a [Value] {
    unsafe { std::slice::from_raw_parts(p.add(1).cast(), (*p).len) }
}
pub unsafe fn enum_descriptor(p: *const EnumData) -> &'static TypeDesc {
    unsafe { &*(*p).core.cast::<TypeDesc>() }
}

pub fn enum_new(
    desc: &'static TypeDesc,
    index: u32,
    fields: &[Value],
) -> Result<*mut EnumData, String> {
    let schema = enum_schema(desc);
    let variant = schema
        .variants
        .get(index as usize)
        .ok_or("invalid enum constructor index")?;
    if variant.fields.len() != fields.len() {
        return Err("invalid enum field count".into());
    }
    for (field, value) in variant.fields.iter().zip(fields) {
        if !accepts(&field.ty, *value) {
            return Err(format!(
                "{}.{}: invalid {} field",
                schema.name, variant.name, field.name
            ));
        }
    }
    let fields: Vec<Value> = variant
        .fields
        .iter()
        .zip(fields)
        .map(|(field, &v)| {
            if field.ty == TypeRef::Int {
                Value::int(crate::error::Int64::of(v).unwrap() as i32)
            } else {
                v
            }
        })
        .collect();
    let roots: Vec<_> = fields.iter().copied().map(Rooted::of).collect();
    let root = Rooted::alloc(
        desc,
        size_of::<EnumData>() + std::mem::size_of_val(fields.as_slice()),
    );
    let p = root.ptr().cast::<EnumData>();
    unsafe {
        (*p).index = index;
        (*p).len = fields.len();
        ptr::copy_nonoverlapping(fields.as_ptr(), p.add(1).cast(), fields.len());
    }
    drop(roots);
    Ok(p)
}
fn accepts(ty: &TypeRef, v: Value) -> bool {
    match ty {
        TypeRef::Int => crate::error::Int64::of(v).is_some_and(|n| i32::try_from(n).is_ok()),
        TypeRef::Int64 => crate::error::Int64::of(v).is_some(),
        TypeRef::Float => v.is_number(),
        TypeRef::Bool => v.is_bool(),
        TypeRef::Str => unsafe { Str::from_value(v).is_some() },
        TypeRef::Buffer => buffer_of(v).is_some(),
        TypeRef::Enum(name) => {
            enum_of(v).is_some_and(|p| unsafe { enum_schema(enum_descriptor(p)).name == *name })
        }
        TypeRef::Dyn => true,
        _ => false,
    }
}
unsafe extern "C" fn trace_enum(p: *mut u8, tracer: *mut Tracer) {
    for v in unsafe { enum_fields(p.cast()) } {
        unsafe {
            (*tracer).mark_value(v.to_bits());
        }
    }
}
unsafe extern "C-unwind" fn enum_len(p: *mut u8, out: *mut usize) -> u8 {
    unsafe {
        *out = (*p.cast::<EnumData>()).len;
    }
    REPLY_OK
}
unsafe extern "C-unwind" fn enum_index(p: *mut u8, key: Value, out: *mut Value) -> u8 {
    let Some(v) = index(key).and_then(|i| unsafe { enum_fields(p.cast()).get(i) }) else {
        return REPLY_MISSING;
    };
    unsafe {
        *out = *v;
    }
    REPLY_OK
}
unsafe extern "C-unwind" fn enum_member(
    p: *mut u8,
    name: crate::symbol::Symbol,
    out: *mut Value,
) -> u8 {
    let e = p.cast::<EnumData>();
    let variant = unsafe { &enum_schema(enum_descriptor(e)).variants[(*e).index as usize] };
    unsafe {
        *out = match name.name() {
            "tag" => Value::int((*e).index as i32),
            "constructor" => Str::value(Str::new(&variant.name)),
            field => match variant.fields.iter().position(|f| f.name == field) {
                Some(i) => enum_fields(e)[i],
                None => return REPLY_MISSING,
            },
        };
    }
    REPLY_OK
}
unsafe extern "C-unwind" fn enum_type_name(p: *mut u8, out: *mut crate::symbol::Symbol) -> u8 {
    unsafe {
        *out = (*enum_descriptor(p.cast()).ext.cast::<EnumType>()).name;
    }
    REPLY_OK
}
static ENUM_PROTO: Protocol = Protocol {
    type_name: Some(enum_type_name),
    len: Some(enum_len),
    index: Some(enum_index),
    get_member: Some(enum_member),
    ..Protocol::NONE
};

/// Translate ABI declaration data once, while loading the plugin.
pub unsafe fn describe_enum(d: &caribou_abi::EnumDesc) -> describe::EnumDesc {
    let variants = if d.variant_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(d.variants, d.variant_count) }
    };
    describe::EnumDesc {
        name: unsafe { d.name.as_str() }.to_owned(),
        variants: variants
            .iter()
            .map(|v| {
                let fields = if v.field_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(v.fields, v.field_count) }
                };
                describe::VariantDesc {
                    name: unsafe { v.name.as_str() }.to_owned(),
                    fields: fields
                        .iter()
                        .map(|f| describe::ParamDesc {
                            name: unsafe { f.name.as_str() }.to_owned(),
                            ty: if f.tag == TypeTag::I64 {
                                TypeRef::Int64
                            } else if f.tag == TypeTag::BUFFER {
                                TypeRef::Buffer
                            } else if f.tag == TypeTag::ENUM {
                                TypeRef::Enum(unsafe { (*f.enumeration).name.as_str() }.to_owned())
                            } else {
                                crate::native::type_ref(f.tag.kind())
                            },
                        })
                        .collect(),
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_share_storage_and_check_bounds() {
        let _lock = heap::gc_guard();
        let p = buffer_new(&[0, 128, 255]);
        let alias = unsafe { buffer_share((*p).bytes, (*p).len) };
        assert_eq!(unsafe { (*p).bytes }, unsafe { (*alias).bytes });
        assert_eq!(
            unsafe { buffer_set(alias.cast(), Value::int(0), Value::int(42)) },
            REPLY_OK
        );
        assert_eq!(unsafe { *(*p).bytes }, 42);
        assert_eq!(
            unsafe { buffer_set(alias.cast(), Value::int(-1), Value::int(1)) },
            REPLY_MISSING
        );
        assert_eq!(
            unsafe { buffer_set(alias.cast(), Value::int(3), Value::int(1)) },
            REPLY_MISSING
        );
        assert_eq!(
            unsafe { buffer_set(alias.cast(), Value::int(0), Value::int(256)) },
            REPLY_MISSING
        );
        assert!(buffer_of(Value::object(p.cast())).is_some());
        assert!(buffer_of(Str::value(Str::new("not bytes"))).is_none());
    }

    #[test]
    fn enum_fields_are_checked_and_integer_inputs_are_normalized() {
        let _lock = heap::gc_guard();
        let schema = describe::EnumDesc {
            name: "test.data.Checked".into(),
            variants: vec![describe::VariantDesc {
                name: "Number".into(),
                fields: vec![describe::ParamDesc {
                    name: "value".into(),
                    ty: TypeRef::Int,
                }],
            }],
        };
        let d = register_enum(schema.clone(), crate::world::LANG_CORE).unwrap();
        assert!(ptr::eq(
            d,
            register_enum(schema.clone(), crate::world::LANG_CORE).unwrap()
        ));
        assert!(enum_new(d, 1, &[]).is_err());
        assert!(enum_new(d, 0, &[]).is_err());
        assert!(enum_new(d, 0, &[Value::bool(true)]).is_err());
        let p = enum_new(d, 0, &[Value::number(42.0)]).unwrap();
        assert_eq!(unsafe { enum_fields(p) }[0].as_int(), Some(42));
        let mut changed = schema;
        changed.variants[0].fields[0].ty = TypeRef::Str;
        assert!(register_enum(changed, crate::world::LANG_CORE).is_err());
    }
}
