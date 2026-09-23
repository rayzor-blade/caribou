//! Binary buffers and enum values owned by the host. Like Text, these
//! carriers are borrowed for a call; use Kept to retain them across calls.
use crate::{Param, Returned, Str, TypeTag, Value, host::host};
use core::{ffi::c_void, marker::PhantomData};

#[repr(C)]
pub struct BufferData {
    pub core: *const c_void,
    pub len: usize,
    pub bytes: *mut u8,
}

/// A shared binary buffer. Construction copies into the host heap once;
/// language crossings share the backing storage.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct Buffer(*const BufferData);

impl Buffer {
    pub const NULL: Self = Self(core::ptr::null());
    pub fn new(bytes: &[u8]) -> Self {
        unsafe { (host().buffer_new)(bytes.as_ptr(), bytes.len()) }
    }
    pub fn of(value: Value) -> Option<Self> {
        let b = unsafe { (host().buffer_of)(value) };
        (!b.0.is_null()).then_some(b)
    }
    /// The pointer must be a live host buffer whose backing bytes remain live.
    pub const unsafe fn from_raw(p: *const BufferData) -> Self {
        Self(p)
    }
    pub fn value(self) -> Value {
        if self.0.is_null() {
            Value::null()
        } else {
            Value::object(self.0.cast())
        }
    }
    /// Borrow the shared backing bytes without copying.
    ///
    /// # Safety
    /// No language may mutate the backing buffer while this slice is live.
    /// Keep the buffer rooted if the borrow spans a call into the host.
    pub unsafe fn as_slice(&self) -> &[u8] {
        if self.is_empty() {
            return &[];
        }
        unsafe { core::slice::from_raw_parts((*self.0).bytes, (*self.0).len) }
    }
    pub fn to_vec(&self) -> alloc::vec::Vec<u8> {
        unsafe { self.as_slice() }.to_vec()
    }
    pub fn len(&self) -> usize {
        if self.0.is_null() {
            0
        } else {
            unsafe { (*self.0).len }
        }
    }
    pub fn as_ptr(&self) -> *const u8 {
        if self.0.is_null() {
            core::ptr::null()
        } else {
            unsafe { (*self.0).bytes }
        }
    }
    pub fn get(&self, index: usize) -> Option<u8> {
        (index < self.len()).then(|| unsafe { *self.as_ptr().add(index) })
    }
    pub fn set(&self, index: usize, value: u8) -> bool {
        if index >= self.len() {
            return false;
        }
        unsafe {
            (*self.0).bytes.add(index).write(value);
        }
        true
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl crate::host::Rootable for Buffer {
    fn value(self) -> Value {
        Buffer::value(self)
    }

    fn of(value: Value) -> Option<Self> {
        Buffer::of(value)
    }
}

#[repr(C)]
pub struct EnumFieldDesc {
    pub name: Str,
    pub tag: TypeTag,
    pub enumeration: *const EnumDesc,
}
unsafe impl Sync for EnumFieldDesc {}

#[repr(C)]
pub struct VariantDesc {
    pub name: Str,
    pub fields: *const EnumFieldDesc,
    pub field_count: usize,
}
unsafe impl Sync for VariantDesc {}

#[repr(C)]
pub struct EnumDesc {
    /// Fully qualified, e.g. window.Event.
    pub name: Str,
    pub variants: *const VariantDesc,
    pub variant_count: usize,
}
unsafe impl Sync for EnumDesc {}

#[repr(C)]
pub struct EnumData {
    pub core: *const c_void,
    pub index: u32,
    pub len: usize,
    // Value fields follow, traced by the host.
}

/// Implemented by `#[derive(PluginEnum)]` and plugin!'s enum declarations.
pub trait PluginEnum: Sized {
    const DESC: &'static EnumDesc;
    fn encode(self) -> Enum<Self>;
    fn decode(value: Enum<Self>) -> Self;
}

/// The one-word C ABI carrier for a Rust enum implementing PluginEnum.
#[repr(transparent)]
pub struct Enum<T: PluginEnum>(*const EnumData, PhantomData<T>);
impl<T: PluginEnum> Copy for Enum<T> {}
impl<T: PluginEnum> Clone for Enum<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: PluginEnum> From<T> for Enum<T> {
    fn from(value: T) -> Self {
        value.encode()
    }
}
impl<T: PluginEnum> Enum<T> {
    pub fn get(self) -> T {
        T::decode(self)
    }
    pub fn value(self) -> Value {
        Value::object(self.0.cast())
    }
    pub fn of(v: Value) -> Option<Self> {
        let p = unsafe { (host().enum_of)(v, T::DESC) };
        (!p.is_null()).then_some(Self(p, PhantomData))
    }
    /// Used by generated constructors. The host validates the tag and fields.
    #[doc(hidden)]
    pub fn new(index: u32, fields: &[Value]) -> Self {
        let p = unsafe { (host().enum_new)(T::DESC, index, fields.as_ptr(), fields.len()) };
        assert!(!p.is_null(), "invalid plugin enum constructor");
        Self(p, PhantomData)
    }
    pub fn index(self) -> u32 {
        unsafe { (*self.0).index }
    }
    pub fn fields(&self) -> &[Value] {
        unsafe { core::slice::from_raw_parts(self.0.add(1).cast(), (*self.0).len) }
    }
}
impl<T: PluginEnum> Param for Enum<T> {
    const TAG: TypeTag = TypeTag::ENUM;
    const ENUM: *const EnumDesc = T::DESC;
}
impl<T: PluginEnum> Returned for Enum<T> {
    const TAG: TypeTag = TypeTag::ENUM;
    const ENUM: *const EnumDesc = T::DESC;
}

/// Types that can appear in enum payloads.
pub trait EnumField: Sized {
    const TAG: TypeTag;
    const ENUM: *const EnumDesc = core::ptr::null();
    fn into_value(self) -> Value;
    fn from_value(v: Value) -> Self;
    /// Visit existing host references before encoding allocates anything.
    /// Native scalars and Rust-owned data contain no such references.
    fn visit(&self, _visit: &mut dyn FnMut(Value)) {}
}

/// Keeps host references embedded in a Rust enum live during encoding.
#[doc(hidden)]
pub struct EnumRoots(alloc::vec::Vec<crate::Kept>);
impl EnumRoots {
    pub fn new(value: &impl EnumField) -> Self {
        let mut roots = Self(alloc::vec::Vec::new());
        value.visit(&mut |v| {
            if v.is_object() {
                roots.0.push(crate::Kept::new(v));
            }
        });
        roots
    }
}
macro_rules! fields {
    ($($ty:ty, $encode:expr, $decode:expr);* $(;)?) => { $(
        impl EnumField for $ty {
            const TAG: TypeTag = <$ty as Param>::TAG;
            fn into_value(self) -> Value { ($encode)(self) }
            fn from_value(v: Value) -> Self { ($decode)(v) }
        }
    )* };
}
fields! {
    i32, Value::int, |v: Value| v.as_int().unwrap();
    u8, |n| Value::int(n as i32), |v: Value| v.as_int().unwrap() as u8;
    u16, |n| Value::int(n as i32), |v: Value| v.as_int().unwrap() as u16;
    i64, |n| unsafe { (host().i64_new)(n) }, |v| unsafe { (host().i64_of)(v) };
    f64, Value::number, |v: Value| v.as_number().unwrap();
    f32, |n| Value::number(n as f64), |v: Value| v.as_number().unwrap() as f32;
    bool, Value::bool, |v: Value| v.as_bool().unwrap();
}
macro_rules! reference_fields {
    ($($ty:ty, $encode:expr, $decode:expr);* $(;)?) => { $(
        impl EnumField for $ty {
            const TAG: TypeTag = <$ty as Param>::TAG;
            fn into_value(self) -> Value { ($encode)(self) }
            fn from_value(v: Value) -> Self { ($decode)(v) }
            fn visit(&self, visit: &mut dyn FnMut(Value)) { visit(($encode)(*self)); }
        }
    )* };
}
reference_fields! {
    Value, |v| v, |v| v;
    crate::Text, crate::Text::value, |v| crate::Text::of(v).unwrap();
    Buffer, Buffer::value, |v| Buffer::of(v).unwrap();
}
impl<T: PluginEnum> EnumField for Enum<T> {
    const TAG: TypeTag = TypeTag::ENUM;
    const ENUM: *const EnumDesc = T::DESC;
    fn into_value(self) -> Value {
        self.value()
    }
    fn from_value(v: Value) -> Self {
        Self::of(v).unwrap()
    }
    fn visit(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.value());
    }
}

// Native strings remain Rust-owned until encoding, then copy into the host
// exactly once. This lets ordinary data enums keep their existing fields.
impl EnumField for alloc::string::String {
    const TAG: TypeTag = <crate::Text as Param>::TAG;
    fn into_value(self) -> Value {
        crate::Text::new(&self).value()
    }
    fn from_value(v: Value) -> Self {
        crate::Text::of(v).unwrap().as_str().into()
    }
}

pub const fn padded_enums(values: &[*const EnumDesc]) -> [*const EnumDesc; crate::MAX_PARAMS] {
    let mut out = [core::ptr::null(); crate::MAX_PARAMS];
    let mut i = 0;
    while i < values.len() {
        out[i] = values[i];
        i += 1;
    }
    out
}

#[doc(hidden)]
#[macro_export]
macro_rules! plugin_enum {
    ($plugin:literal, $name:ident, $( $variant:ident $( ( $( $field:ident : $ty:ty ),* ) )? ; )*) => {
        pub enum $name { $( $variant $( ( $($ty),* ) )? ),* }
        impl $crate::PluginEnum for $name {
            const DESC: &'static $crate::EnumDesc = &$crate::EnumDesc {
                name: $crate::Str::new(concat!($plugin, ".", stringify!($name))),
                variants: &[$($crate::data::VariantDesc {
                    name: $crate::Str::new(stringify!($variant)),
                    fields: &[$($($crate::data::EnumFieldDesc {
                        name: $crate::Str::new(stringify!($field)),
                        tag: <$ty as $crate::EnumField>::TAG,
                        enumeration: <$ty as $crate::EnumField>::ENUM,
                    }),*)?] as *const _,
                    field_count: 0 $( $(+ {let _ = stringify!($field); 1})* )?,
                }),*] as *const _,
                variant_count: $crate::plugin!(@count $($variant)*),
            };
            fn encode(self) -> $crate::Enum<Self> {
                let _inputs = $crate::data::EnumRoots::new(&self);
                #[allow(non_camel_case_types)]
                enum Index { $($variant),* }
                match self { $(Self::$variant $( ( $($field),* ) )? => {
                    // Root each converted field before converting the next.
                    let roots: [$crate::Kept; 0 $( $(+ {let _ = stringify!($field); 1})* )?] =
                        [$($($crate::Kept::new($crate::EnumField::into_value($field))),*)?];
                    let values = roots.each_ref().map(|v| v.get());
                    $crate::Enum::new(Index::$variant as u32, &values)
                }),* }
            }
            fn decode(value: $crate::Enum<Self>) -> Self {
                #[allow(non_camel_case_types)]
                enum Index { $($variant),* }
                match value.index() {
                    $(i if i == Index::$variant as u32 => {
                        #[allow(unused_mut, unused_variables)]
                        let mut fields = value.fields().iter();
                        Self::$variant $( ( $(<$ty as $crate::EnumField>::from_value(*fields.next().unwrap())),* ) )?
                    }),*,
                    _ => unreachable!("host validated enum tag"),
                }
            }
        }
        impl $crate::EnumField for $name {
            const TAG: $crate::TypeTag = $crate::TypeTag::ENUM;
            const ENUM: *const $crate::EnumDesc = <Self as $crate::PluginEnum>::DESC;
            fn into_value(self) -> $crate::Value {
                <Self as $crate::PluginEnum>::encode(self).value()
            }
            fn from_value(value: $crate::Value) -> Self {
                $crate::Enum::<Self>::of(value).expect("host validated enum field").get()
            }
            fn visit(&self, visit: &mut dyn FnMut($crate::Value)) {
                match self { $(Self::$variant $( ( $($field),* ) )? => {
                    $($($crate::EnumField::visit($field, visit);)*)?
                }),* }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    crate::plugin_enum!("test", Event, Closed; Data(bytes: Buffer, count: i64););

    #[test]
    fn carriers_and_enum_metadata_match_the_abi() {
        assert_eq!(size_of::<Buffer>(), size_of::<usize>());
        assert_eq!(size_of::<Enum<Event>>(), size_of::<usize>());
        assert_eq!(core::mem::offset_of!(BufferData, core), 0);
        assert_eq!(core::mem::offset_of!(EnumData, core), 0);
        assert_eq!(size_of::<EnumData>() % align_of::<Value>(), 0);
        assert_eq!(Event::DESC.variant_count, 2);
        assert_eq!(unsafe { Event::DESC.name.as_str() }, "test.Event");
        let payload = unsafe { &*Event::DESC.variants.add(1) };
        assert_eq!(payload.field_count, 2);
        assert_eq!(unsafe { (*payload.fields).tag }, TypeTag::BUFFER);
        assert_eq!(unsafe { (*payload.fields.add(1)).tag }, TypeTag::I64);
        assert_eq!(<Enum<Event> as Param>::TAG, TypeTag::ENUM);
    }
}
