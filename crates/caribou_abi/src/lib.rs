//! Layouts, constants and the plugin boundary shared by every runtime, plugin
//! and the core. No runtime dependencies or symbol definitions. Runtime
//! allocations go through the host; explicit Rust copies use `alloc`. HashLink layouts keep `hl.h`'s names; sizes and offsets are
//! asserted in the tests. 64-bit targets only.

#![no_std]
#![allow(non_camel_case_types, non_snake_case, clippy::missing_safety_doc)]

#[cfg(test)]
extern crate std;
// For the `Box` a plugin returns an object as: the type alone, nothing
// allocated here.
extern crate alloc;

use core::ffi::{c_char, c_int, c_uint, c_void};

/// Bumped on any change to a layout, a discriminant, a signature or the
/// meaning of a flag defined in this crate. The core compares its own copy
/// against a plugin's before binding a single symbol.
pub const ABI_VERSION: u32 = 4;

/// Every plugin exports `extern "C" fn caribou_abi_version() -> u32`.
pub const ABI_VERSION_SYMBOL: &str = "caribou_abi_version";

/// Every plugin exports `extern "C" fn caribou_plugin_entry(*const host::Host)
/// -> *const PluginInfo`: the core hands its table in and takes the
/// plugin's out.
pub const PLUGIN_ENTRY_SYMBOL: &str = "caribou_plugin_entry";

pub mod host;
pub use host::{Future, Kept, Rootable, Rooted, Text};
pub mod data;
pub use caribou_abi_derive::PluginEnum;
pub use data::{Buffer, Enum, EnumDesc, EnumField, PluginEnum};

/// Which runtime defines a type's semantics. A registry, not an enum: the
/// core assigns ids at world start, one per adapter and one per Zyntax
/// grammar snapshot.
pub type LangId = u32;

// ---------------------------------------------------------------------------
// HashLink layouts
// ---------------------------------------------------------------------------

/// The HashLink C layouts, from `hl.h`, for 64-bit targets.
pub mod hl {
    use super::*;

    /// A UTF-16 code unit. `wchar_t` on Windows, `char16_t` elsewhere; both
    /// are 16 bits wide.
    pub type uchar = u16;
    pub type vbyte = u8;
    pub const HL_WSIZE: usize = 8;

    /// C enum of `int` width: `unsigned` under clang, signed under MSVC.
    /// Always spell kinds through this alias, never as bare integers.
    #[cfg(target_env = "msvc")]
    pub type hl_type_kind = c_int;
    #[cfg(not(target_env = "msvc"))]
    pub type hl_type_kind = c_uint;

    pub const HVOID: hl_type_kind = 0;
    pub const HUI8: hl_type_kind = 1;
    pub const HUI16: hl_type_kind = 2;
    pub const HI32: hl_type_kind = 3;
    pub const HI64: hl_type_kind = 4;
    pub const HF32: hl_type_kind = 5;
    pub const HF64: hl_type_kind = 6;
    pub const HBOOL: hl_type_kind = 7;
    pub const HBYTES: hl_type_kind = 8;
    pub const HDYN: hl_type_kind = 9;
    pub const HFUN: hl_type_kind = 10;
    pub const HOBJ: hl_type_kind = 11;
    pub const HARRAY: hl_type_kind = 12;
    pub const HTYPE: hl_type_kind = 13;
    pub const HREF: hl_type_kind = 14;
    pub const HVIRTUAL: hl_type_kind = 15;
    pub const HDYNOBJ: hl_type_kind = 16;
    pub const HABSTRACT: hl_type_kind = 17;
    pub const HENUM: hl_type_kind = 18;
    pub const HNULL: hl_type_kind = 19;
    pub const HMETHOD: hl_type_kind = 20;
    pub const HSTRUCT: hl_type_kind = 21;
    pub const HPACKED: hl_type_kind = 22;
    pub const HLAST: hl_type_kind = 23;

    /// `hl_type_size`: the byte size of a value of this kind. Kinds from
    /// `HBYTES` up are pointers.
    pub const fn type_size(kind: hl_type_kind) -> usize {
        match kind {
            HVOID => 0,
            HUI8 | HBOOL => 1,
            HUI16 => 2,
            HI32 | HF32 => 4,
            HI64 | HF64 => 8,
            _ => HL_WSIZE,
        }
    }

    /// `hl_is_ptr`.
    pub const fn is_ptr(kind: hl_type_kind) -> bool {
        kind >= HBYTES
    }

    #[repr(C)]
    pub struct hl_alloc_block {
        _private: [u8; 0],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct hl_alloc {
        pub cur: *mut hl_alloc_block,
    }

    #[repr(C)]
    pub struct hl_module_context {
        pub alloc: hl_alloc,
        pub functions_ptrs: *mut *mut c_void,
        pub functions_types: *mut *mut hl_type,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct hl_type_fun_closure_type {
        pub kind: hl_type_kind,
        pub p: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct hl_type_fun_closure {
        pub args: *mut *mut hl_type,
        pub ret: *mut hl_type,
        pub nargs: c_int,
        pub parent: *mut hl_type,
    }

    #[repr(C)]
    pub struct hl_type_fun {
        pub args: *mut *mut hl_type,
        pub ret: *mut hl_type,
        pub nargs: c_int,
        pub parent: *mut hl_type,
        pub closure_type: hl_type_fun_closure_type,
        pub closure: hl_type_fun_closure,
    }

    #[repr(C)]
    pub struct hl_obj_field {
        pub name: *const uchar,
        pub t: *mut hl_type,
        pub hashed_name: c_int,
    }

    #[repr(C)]
    pub struct hl_obj_proto {
        pub name: *const uchar,
        pub findex: c_int,
        pub pindex: c_int,
        pub hashed_name: c_int,
    }

    #[repr(C)]
    pub struct hl_type_obj {
        pub nfields: c_int,
        pub nproto: c_int,
        pub nbindings: c_int,
        pub name: *const uchar,
        pub super_: *mut hl_type,
        pub fields: *mut hl_obj_field,
        pub proto: *mut hl_obj_proto,
        pub bindings: *mut c_int,
        pub global_value: *mut *mut c_void,
        pub m: *mut hl_module_context,
        pub rt: *mut hl_runtime_obj,
    }

    #[repr(C)]
    pub struct hl_field_lookup {
        pub t: *mut hl_type,
        pub hashed_name: c_int,
        /// Negative or zero: index into methods.
        pub field_index: c_int,
    }

    #[repr(C)]
    pub struct hl_type_virtual {
        pub fields: *mut hl_obj_field,
        pub nfields: c_int,
        pub dataSize: c_int,
        pub indexes: *mut c_int,
        pub lookup: *mut hl_field_lookup,
    }

    #[repr(C)]
    pub struct hl_enum_construct {
        pub name: *const uchar,
        pub nparams: c_int,
        pub params: *mut *mut hl_type,
        pub size: c_int,
        pub hasptr: bool,
        pub offsets: *mut c_int,
    }

    #[repr(C)]
    pub struct hl_type_enum {
        pub name: *const uchar,
        pub nconstructs: c_int,
        pub constructs: *mut hl_enum_construct,
        pub global_value: *mut *mut c_void,
    }

    /// The per-kind detail of an `hl_type`, selected by `kind`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union hl_type_detail {
        pub abs_name: *const uchar,
        pub fun: *mut hl_type_fun,
        pub obj: *mut hl_type_obj,
        pub tenum: *mut hl_type_enum,
        pub virt: *mut hl_type_virtual,
        pub tparam: *mut hl_type,
    }

    /// Word zero of every heap object points at one of these. A `TypeDesc`
    /// begins with exactly this struct; its tail is invisible to C code.
    #[repr(C)]
    pub struct hl_type {
        pub kind: hl_type_kind,
        pub detail: hl_type_detail,
        pub vobj_proto: *mut *mut c_void,
        pub mark_bits: *mut c_uint,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union vdynamic_value {
        pub b: bool,
        pub ui8: u8,
        pub ui16: u16,
        pub i: c_int,
        pub f: f32,
        pub d: f64,
        pub bytes: *mut vbyte,
        pub ptr: *mut c_void,
        pub i64_: i64,
    }

    #[repr(C)]
    pub struct vdynamic {
        pub t: *mut hl_type,
        pub v: vdynamic_value,
    }

    /// Fields follow the header inline, at the offsets `hl_runtime_obj`
    /// records.
    #[repr(C)]
    pub struct vobj {
        pub t: *mut hl_type,
    }

    #[repr(C)]
    pub struct vvirtual {
        pub t: *mut hl_type,
        pub value: *mut vdynamic,
        pub next: *mut vvirtual,
    }

    /// `hl_vfields`: a virtual's field slots start right after the header.
    pub unsafe fn vfields(v: *mut vvirtual) -> *mut *mut c_void {
        unsafe { v.add(1) as *mut *mut c_void }
    }

    /// Elements follow the header.
    #[repr(C)]
    pub struct varray {
        pub t: *mut hl_type,
        pub at: *mut hl_type,
        pub size: c_int,
        pub __pad: c_int,
    }

    /// `hl_aptr`: the element base of an array.
    pub unsafe fn aptr<T>(a: *mut varray) -> *mut T {
        unsafe { a.add(1) as *mut T }
    }

    #[repr(C)]
    pub struct vclosure {
        pub t: *mut hl_type,
        pub fun: *mut c_void,
        pub hasValue: c_int,
        pub stackCount: c_int,
        pub value: *mut c_void,
    }

    #[repr(C)]
    pub struct vclosure_wrapper {
        pub cl: vclosure,
        pub wrappedFun: *mut vclosure,
    }

    #[repr(C)]
    pub struct hl_runtime_binding {
        pub ptr: *mut c_void,
        pub closure: *mut hl_type,
        pub fid: c_int,
    }

    #[repr(C)]
    pub struct hl_runtime_obj {
        pub t: *mut hl_type,
        pub nfields: c_int,
        pub nproto: c_int,
        pub size: c_int,
        pub nmethods: c_int,
        pub nbindings: c_int,
        pub hasPtr: bool,
        /// Ash additions inside upstream's padding; offsets and size unchanged.
        pub pad_size: u8,
        pub largest_field: u8,
        pub methods: *mut *mut c_void,
        pub fields_indexes: *mut c_int,
        pub bindings: *mut hl_runtime_binding,
        pub parent: *mut hl_runtime_obj,
        pub toStringFun: Option<unsafe extern "C" fn(*mut vdynamic) -> *const uchar>,
        pub compareFun: Option<unsafe extern "C" fn(*mut vdynamic, *mut vdynamic) -> c_int>,
        pub castFun: Option<unsafe extern "C" fn(*mut vdynamic, *mut hl_type) -> *mut vdynamic>,
        pub getFieldFun: Option<unsafe extern "C" fn(*mut vdynamic, c_int) -> *mut vdynamic>,
        pub nlookup: c_int,
        pub ninterfaces: c_int,
        pub lookup: *mut hl_field_lookup,
        pub interfaces: *mut c_int,
    }

    #[repr(C)]
    pub struct vdynobj {
        pub t: *mut hl_type,
        pub lookup: *mut hl_field_lookup,
        pub raw_data: *mut c_char,
        pub values: *mut *mut c_void,
        pub nfields: c_int,
        pub raw_size: c_int,
        pub nvalues: c_int,
        pub virtuals: *mut vvirtual,
    }

    pub const HL_DYNOBJ_INDEX_SHIFT: u32 = 17;
    pub const HL_DYNOBJ_INDEX_MASK: u32 = (1 << HL_DYNOBJ_INDEX_SHIFT) - 1;

    #[repr(C)]
    pub struct venum {
        pub t: *mut hl_type,
        pub index: c_int,
    }

    /// Haxe's `String`: UCS-2 bytes and a length in code units.
    #[repr(C)]
    pub struct vstring {
        pub t: *mut hl_type,
        pub bytes: *mut uchar,
        pub length: c_int,
    }

    /// The `DEFINE_PRIM` resolver an HDLL exports for each primitive: writes
    /// the signature string through `sign` and returns the function itself.
    /// Never call the resolver as the primitive.
    pub type PrimResolver = unsafe extern "C" fn(sign: *mut *const c_char) -> *mut c_void;

    /// Emit a `DEFINE_PRIM` resolver named `$resolver` for `$function`.
    #[macro_export]
    macro_rules! define_prim {
        ($resolver:ident, $function:ident, $signature:literal) => {
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $resolver(
                sign: *mut *const ::core::ffi::c_char,
            ) -> *mut ::core::ffi::c_void {
                if !sign.is_null() {
                    unsafe {
                        *sign = concat!($signature, "\0").as_ptr() as *const ::core::ffi::c_char;
                    }
                }
                $function as *mut ::core::ffi::c_void
            }
        };
    }
}

// ---------------------------------------------------------------------------
// Allocation kinds
// ---------------------------------------------------------------------------

/// HashLink's `MEM_KIND_*` and `MEM_*` flags, as `hl_gc_alloc_gen` receives
/// them. The core honours all of them: `Typed` may be traced through its
/// descriptor, `Raw` is scanned conservatively, `NoPtr` is never scanned,
/// `Finalizer` is raw with a callback in word zero.
pub mod mem {
    pub const KIND_DYNAMIC: u32 = 0;
    pub const KIND_RAW: u32 = 1;
    pub const KIND_NOPTR: u32 = 2;
    pub const KIND_FINALIZER: u32 = 3;
    pub const KIND_MASK: u32 = 3;
    /// With `KIND_DYNAMIC`: word zero is the core's `TypeDesc`, not a bare
    /// `hl_type`, so the collector may read its trace and drop hooks. A bare
    /// `hl_type` has no tail to read; leave this clear for one. Bit 2 is
    /// unused by HashLink.
    pub const TRACED: u32 = 4;
    pub const ALIGN_DOUBLE: u32 = 128;
    pub const ZERO: u32 = 256;

    #[repr(u32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AllocKind {
        /// Word zero is a type descriptor. With [`TRACED`] it is traced and
        /// dropped through the descriptor's hooks; without, or when the
        /// descriptor has no trace hook, it is scanned conservatively.
        Typed = KIND_DYNAMIC,
        /// Scanned conservatively. What every HDLL gets by default.
        Raw = KIND_RAW,
        /// Never scanned: pixels, audio, mesh data, string bytes.
        NoPtr = KIND_NOPTR,
        /// Raw, with `unsafe extern "C" fn(*mut c_void)` in word zero, called
        /// once nothing can reach the block.
        Finalizer = KIND_FINALIZER,
    }

    impl AllocKind {
        pub const fn from_flags(flags: u32) -> AllocKind {
            match flags & KIND_MASK {
                KIND_RAW => AllocKind::Raw,
                KIND_NOPTR => AllocKind::NoPtr,
                KIND_FINALIZER => AllocKind::Finalizer,
                _ => AllocKind::Typed,
            }
        }
    }

    /// Word zero of a `Finalizer` block.
    pub type Finalizer = unsafe extern "C" fn(*mut core::ffi::c_void);
}

// ---------------------------------------------------------------------------
// The dynamic-ABI value
// ---------------------------------------------------------------------------

/// One 64-bit NaN-boxed value, the dynamic-ABI currency. Typed HashLink code
/// never sees one.
///
/// Layout:
///
/// ```text
/// number      any f64 whose bits do not have every QNAN bit set
/// null        0x7FFC_0000_0000_0000
/// false       0x7FFC_0000_0000_0001
/// true        0x7FFC_0000_0000_0002
/// undefined   0x7FFC_0000_0000_0003   (internal sentinel, never a language value)
/// i32         0x7FFD_0000_0000_0000 | (n as u32)
/// object      0xFFFC_0000_0000_0000 | (ptr & 0x0000_FFFF_FFFF_FFFF)
/// ```
///
/// WrenLift's layout plus the integer tag. Pointers are 48-bit user
/// addresses, kept and read zero-extended: Linux on arm64 hands out the
/// whole range, bit 47 included.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value(u64);

impl Value {
    pub const QNAN: u64 = 0x7FFC_0000_0000_0000;
    pub const SIGN_BIT: u64 = 1 << 63;
    pub const TAG_NULL: u64 = Self::QNAN;
    pub const TAG_FALSE: u64 = Self::QNAN | 1;
    pub const TAG_TRUE: u64 = Self::QNAN | 2;
    pub const TAG_UNDEFINED: u64 = Self::QNAN | 3;
    pub const TAG_I32: u64 = Self::QNAN | (1 << 48);
    pub const TAG_OBJ: u64 = Self::SIGN_BIT | Self::QNAN;
    pub const PTR_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
    const TAG_MASK: u64 = Self::SIGN_BIT | Self::QNAN | (0xF << 48);

    #[inline]
    pub const fn from_bits(bits: u64) -> Value {
        Value(bits)
    }
    #[inline]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    #[inline]
    pub const fn null() -> Value {
        Value(Self::TAG_NULL)
    }
    #[inline]
    pub const fn undefined() -> Value {
        Value(Self::TAG_UNDEFINED)
    }
    #[inline]
    pub const fn bool(b: bool) -> Value {
        Value(if b { Self::TAG_TRUE } else { Self::TAG_FALSE })
    }
    /// NaNs are canonicalised so no number collides with a tag.
    #[inline]
    pub fn number(n: f64) -> Value {
        if n.is_nan() {
            Value(0x7FF8_0000_0000_0000)
        } else {
            Value(n.to_bits())
        }
    }
    #[inline]
    pub const fn int(n: i32) -> Value {
        Value(Self::TAG_I32 | (n as u32 as u64))
    }
    #[inline]
    pub fn object(ptr: *const c_void) -> Value {
        Value(Self::TAG_OBJ | (ptr as usize as u64 & Self::PTR_MASK))
    }

    #[inline]
    pub const fn is_number(self) -> bool {
        (self.0 & Self::QNAN) != Self::QNAN
    }
    #[inline]
    pub const fn is_null(self) -> bool {
        self.0 == Self::TAG_NULL
    }
    #[inline]
    pub const fn is_undefined(self) -> bool {
        self.0 == Self::TAG_UNDEFINED
    }
    #[inline]
    pub const fn is_bool(self) -> bool {
        self.0 == Self::TAG_TRUE || self.0 == Self::TAG_FALSE
    }
    #[inline]
    pub const fn is_int(self) -> bool {
        (self.0 & Self::TAG_MASK) == Self::TAG_I32
    }
    #[inline]
    pub const fn is_object(self) -> bool {
        (self.0 & Self::TAG_OBJ) == Self::TAG_OBJ
    }

    #[inline]
    pub fn as_number(self) -> Option<f64> {
        if self.is_number() {
            Some(f64::from_bits(self.0))
        } else {
            None
        }
    }
    #[inline]
    pub const fn as_bool(self) -> Option<bool> {
        match self.0 {
            Self::TAG_TRUE => Some(true),
            Self::TAG_FALSE => Some(false),
            _ => None,
        }
    }
    #[inline]
    pub const fn as_int(self) -> Option<i32> {
        if self.is_int() {
            Some(self.0 as u32 as i32)
        } else {
            None
        }
    }
    #[inline]
    pub fn as_object(self) -> Option<*mut c_void> {
        if self.is_object() {
            Some((self.0 & Self::PTR_MASK) as usize as *mut c_void)
        } else {
            None
        }
    }
    /// Wren's truthiness, which the protocol adopts: only `false` and `null`
    /// are falsy.
    #[inline]
    pub const fn is_truthy(self) -> bool {
        self.0 != Self::TAG_FALSE && self.0 != Self::TAG_NULL
    }
}

impl core::fmt::Debug for Value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(n) = self.as_number() {
            write!(f, "Value::number({n})")
        } else if let Some(i) = self.as_int() {
            write!(f, "Value::int({i})")
        } else if let Some(b) = self.as_bool() {
            write!(f, "Value::bool({b})")
        } else if self.is_null() {
            write!(f, "Value::null")
        } else if self.is_undefined() {
            write!(f, "Value::undefined")
        } else if let Some(p) = self.as_object() {
            write!(f, "Value::object({p:p})")
        } else {
            write!(f, "Value(0x{:016x})", self.0)
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The closed, language-neutral list every error value answers with.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Runtime = 0,
    Type = 1,
    NullAccess = 2,
    Index = 3,
    Arithmetic = 4,
    User = 5,
    Cancelled = 6,
    StackOverflow = 7,
    OutOfMemory = 8,
    /// A plugin panicked, or the runtime broke an invariant of its own.
    Internal = 9,
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

/// Borrowed UTF-8 with an explicit length; lets descriptor tables be `static`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

impl Str {
    pub const EMPTY: Str = Str {
        ptr: core::ptr::null(),
        len: 0,
    };

    pub const fn new(s: &'static str) -> Str {
        Str {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }

    /// # Safety
    /// `ptr` must point at `len` bytes of UTF-8 that outlive the returned
    /// borrow.
    pub const unsafe fn as_str<'a>(self) -> &'a str {
        if self.len == 0 {
            return "";
        }
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(self.ptr, self.len)) }
    }
}

unsafe impl Sync for Str {}
unsafe impl Send for Str {}

/// A plugin signature tag. The base tags follow `hl_type_kind`; BUFFER
/// and ENUM are plugin-only tags resolved to core descriptors by the loader.
/// Scalars pass raw, Text and data carriers as pointers, DYN as Value bits.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeTag(pub u8);

impl TypeTag {
    pub const VOID: TypeTag = TypeTag(hl::HVOID as u8);
    pub const UI8: TypeTag = TypeTag(hl::HUI8 as u8);
    pub const UI16: TypeTag = TypeTag(hl::HUI16 as u8);
    pub const I32: TypeTag = TypeTag(hl::HI32 as u8);
    pub const I64: TypeTag = TypeTag(hl::HI64 as u8);
    pub const F32: TypeTag = TypeTag(hl::HF32 as u8);
    pub const F64: TypeTag = TypeTag(hl::HF64 as u8);
    pub const BOOL: TypeTag = TypeTag(hl::HBOOL as u8);
    pub const BYTES: TypeTag = TypeTag(hl::HBYTES as u8);
    pub const DYN: TypeTag = TypeTag(hl::HDYN as u8);
    pub const FUN: TypeTag = TypeTag(hl::HFUN as u8);
    pub const OBJ: TypeTag = TypeTag(hl::HOBJ as u8);
    pub const ARRAY: TypeTag = TypeTag(hl::HARRAY as u8);
    pub const BUFFER: TypeTag = TypeTag(24);
    pub const ENUM: TypeTag = TypeTag(25);
    pub const FUTURE: TypeTag = TypeTag(26);
    pub const ABSTRACT: TypeTag = TypeTag(hl::HABSTRACT as u8);

    pub const fn kind(self) -> hl::hl_type_kind {
        self.0 as hl::hl_type_kind
    }
}

pub const MAX_PARAMS: usize = 16;

/// Flags on a [`SymbolDesc`].
pub mod sym {
    /// A static method or free function: no receiver in slot zero.
    pub const STATIC: u32 = 1 << 0;
    /// Every parameter and the return are boxed [`super::Value`]s,
    /// whatever the tags say.
    pub const DYNAMIC: u32 = 1 << 1;
    /// Trailing arguments arrive as one array.
    pub const VARIADIC: u32 = 1 << 2;
    /// The call may park the fiber or perform an effect; the bridge must not
    /// hold anything across it that a switch would invalidate.
    pub const EFFECTFUL: u32 = 1 << 3;
}

/// No class: what a parameter or result that is not an object carries in
/// [`SymbolDesc::param_classes`] and [`SymbolDesc::ret_class`].
pub const NO_CLASS: u8 = u8::MAX;

/// One entry in a plugin's table: a native function, where it hangs in a
/// class namespace, and its typed signature. A parameter or result
/// tagged [`TypeTag::OBJ`] names its class by index into the plugin's
/// [`ClassDesc`] table; a result of a class is a new object of it, owned
/// by the core from then on, and a parameter of one is borrowed for the
/// call.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SymbolDesc {
    /// Empty for a free function.
    pub class: Str,
    pub method: Str,
    pub func: *const c_void,
    pub flags: u32,
    pub param_count: u8,
    pub ret: TypeTag,
    pub params: [TypeTag; MAX_PARAMS],
    pub ret_class: u8,
    pub param_classes: [u8; MAX_PARAMS],
    pub ret_enum: *const EnumDesc,
    pub param_enums: [*const EnumDesc; MAX_PARAMS],
    /// The value produced by a FUTURE result. VOID for other results.
    pub future_ret: TypeTag,
    pub future_ret_class: u8,
    pub future_ret_enum: *const EnumDesc,
}

unsafe impl Sync for SymbolDesc {}
unsafe impl Send for SymbolDesc {}

/// A class of a plugin whose instances cross: what the core calls to
/// release one it no longer holds. The payload is the plugin's memory;
/// the core reads nothing of it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClassDesc {
    pub name: Str,
    pub drop: Option<unsafe extern "C" fn(*mut c_void)>,
}

unsafe impl Sync for ClassDesc {}
unsafe impl Send for ClassDesc {}

/// What `caribou_plugin_entry` returns. The core reads `abi_version` first
/// and binds nothing on a mismatch.
#[repr(C)]
pub struct PluginInfo {
    pub abi_version: u32,
    pub name: Str,
    pub symbols: *const SymbolDesc,
    pub symbol_count: usize,
    pub classes: *const ClassDesc,
    pub class_count: usize,
    pub enums: *const *const EnumDesc,
    pub enum_count: usize,
}

unsafe impl Sync for PluginInfo {}
unsafe impl Send for PluginInfo {}

// ---------------------------------------------------------------------------
// Writing a plugin
// ---------------------------------------------------------------------------

/// A type of a plugin whose instances cross: named in the plugin's class
/// table, implemented by [`plugin!`] for each `class` it declares.
pub trait PluginClass {
    const NAME: &'static str;
    const TYPE_NAME: &'static str;
}

/// A Rust type a plugin function takes, and how it crosses: a scalar or a
/// `Value` by its tag; a [`Text`] as a core string, borrowed for the call;
/// `&T` or `&mut T` of a [`PluginClass`] as an object of that class,
/// borrowed for the call.
pub trait Param {
    const TAG: TypeTag;
    /// The class's name for an object, else `None`.
    const CLASS: Option<&'static str> = None;
    const ENUM: *const EnumDesc = core::ptr::null();
}

/// A Rust type a plugin function returns: a scalar or a `Value` by its
/// tag; a [`Text`] made by [`host::text`]; `Box<T>` of a [`PluginClass`]
/// as a new object of that class, owned by the core from then on.
pub trait Returned {
    const TAG: TypeTag;
    const CLASS: Option<&'static str> = None;
    const ENUM: *const EnumDesc = core::ptr::null();
    const FUTURE_TAG: TypeTag = TypeTag::VOID;
    const FUTURE_CLASS: Option<&'static str> = None;
    const FUTURE_ENUM: *const EnumDesc = core::ptr::null();
}

/// A value type carried by a typed [`Future`].
pub trait FutureResult {
    const TAG: TypeTag;
    const CLASS: Option<&'static str> = None;
    const ENUM: *const EnumDesc = core::ptr::null();
}

macro_rules! tagged {
    ($($ty:ty => $tag:expr),* $(,)?) => {
        $(
            impl Param for $ty {
                const TAG: TypeTag = $tag;
            }
            impl Returned for $ty {
                const TAG: TypeTag = $tag;
            }
        )*
    };
}

tagged! {
    () => TypeTag::VOID,
    u8 => TypeTag::UI8,
    u16 => TypeTag::UI16,
    i32 => TypeTag::I32,
    i64 => TypeTag::I64,
    f32 => TypeTag::F32,
    f64 => TypeTag::F64,
    bool => TypeTag::BOOL,
    Value => TypeTag::DYN,
    Text => TypeTag::BYTES,
    Buffer => TypeTag::BUFFER,
}

macro_rules! future_results {
    ($($ty:ty),* $(,)?) => { $(
        impl FutureResult for $ty {
            const TAG: TypeTag = <$ty as Returned>::TAG;
            const CLASS: Option<&'static str> = <$ty as Returned>::CLASS;
            const ENUM: *const EnumDesc = <$ty as Returned>::ENUM;
        }
    )* };
}
future_results!((), u8, u16, i32, i64, f32, f64, bool, Value, Text, Buffer);

impl<T: PluginClass> FutureResult for T {
    const TAG: TypeTag = TypeTag::OBJ;
    const CLASS: Option<&'static str> = Some(T::NAME);
}

impl<T: PluginEnum> FutureResult for Enum<T> {
    const TAG: TypeTag = TypeTag::ENUM;
    const ENUM: *const EnumDesc = T::DESC;
}

impl<T: FutureResult> Param for Future<T> {
    const TAG: TypeTag = TypeTag::FUTURE;
}

impl<T: FutureResult> Returned for Future<T> {
    const TAG: TypeTag = TypeTag::FUTURE;
    const FUTURE_TAG: TypeTag = T::TAG;
    const FUTURE_CLASS: Option<&'static str> = T::CLASS;
    const FUTURE_ENUM: *const EnumDesc = T::ENUM;
}

impl<T: PluginClass> Param for &T {
    const TAG: TypeTag = TypeTag::OBJ;
    const CLASS: Option<&'static str> = Some(T::NAME);
}

impl<T: PluginClass> Param for &mut T {
    const TAG: TypeTag = TypeTag::OBJ;
    const CLASS: Option<&'static str> = Some(T::NAME);
}

impl<T: PluginClass> Returned for alloc::boxed::Box<T> {
    const TAG: TypeTag = TypeTag::OBJ;
    const CLASS: Option<&'static str> = Some(T::NAME);
}

/// `tags` at the front of a full parameter list, for a [`SymbolDesc`].
pub const fn padded(tags: &[TypeTag]) -> [TypeTag; MAX_PARAMS] {
    let mut out = [TypeTag::VOID; MAX_PARAMS];
    let mut i = 0;
    while i < tags.len() {
        out[i] = tags[i];
        i += 1;
    }
    out
}

/// `classes` at the front of a full list, [`NO_CLASS`] after.
pub const fn padded_classes(classes: &[u8]) -> [u8; MAX_PARAMS] {
    let mut out = [NO_CLASS; MAX_PARAMS];
    let mut i = 0;
    while i < classes.len() {
        out[i] = classes[i];
        i += 1;
    }
    out
}

const fn same_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[doc(hidden)]
pub const fn is_class(class: Option<&str>, expected: &str) -> bool {
    match class {
        Some(class) => same_str(class, expected),
        None => false,
    }
}

/// The index of the class named `class` in `table`, [`NO_CLASS`] for
/// none; a class a signature names but the plugin never declared is a
/// compile-time error.
pub const fn class_index(table: &[ClassDesc], class: Option<&str>) -> u8 {
    let Some(class) = class else {
        return NO_CLASS;
    };
    let mut i = 0;
    while i < table.len() {
        if same_str(unsafe { table[i].name.as_str() }, class) {
            return i as u8;
        }
        i += 1;
    }
    panic!("a signature names a class the plugin does not declare")
}

/// The finalizer [`plugin!`] writes for a class: the box the constructor
/// returned, dropped.
///
/// # Safety
/// `p` is a payload a `Box<T>` returned to the core, freed once.
pub unsafe extern "C" fn drop_boxed<T>(p: *mut c_void) {
    drop(unsafe { alloc::boxed::Box::from_raw(p as *mut T) });
}

/// A plugin's table: its name, and the functions it exports, declared
/// by signature the way a header declares them. The functions are
/// ordinary items of the crate, `extern "C"` over the types [`Param`] and
/// [`Returned`] cover; a `class` names a type whose associated functions
/// hang in that class, and whose instances cross as objects when a
/// signature takes `&T`/`&mut T` or returns `Box<T>`; a function outside
/// any class is a static of a class named after the plugin. Each
/// declaration is checked against the item it names, so the two cannot
/// drift. The macro writes the tables, the finalizer of each class, the
/// entry, which keeps the core's table for [`host`], and the version
/// symbol.
///
/// ```ignore
/// pub extern "C" fn hypot(a: f64, b: f64) -> f64 { a.hypot(b) }
///
/// pub struct Vec2 { x: f64, y: f64 }
/// impl Vec2 {
///     pub extern "C" fn new(x: f64, y: f64) -> Box<Vec2> { Box::new(Vec2 { x, y }) }
///     pub extern "C" fn len(this: &Vec2) -> f64 { this.x.hypot(this.y) }
/// }
///
/// caribou_abi::plugin! {
///     name: "math";
///     fn hypot(f64, f64) -> f64;
///     class Vec2 {
///         fn new(f64, f64) -> Box<Vec2>;
///         fn len(&Vec2) -> f64;
///     }
/// }
/// ```
#[macro_export]
macro_rules! plugin {
    (name: $name:literal ; $($rest:tt)*) => {
        $crate::plugin!(@munch $name [] [] [] $($rest)*);
    };
    // A free function.
    (@munch $name:literal [$($acc:tt)*] [$($classes:tt)*] [$($enums:ident)*]
        fn $method:ident ( $($ty:ty),* $(,)? ) $(-> $ret:ty)? ;
        $($rest:tt)*
    ) => {
        $crate::plugin!(@munch $name [$($acc)* { "" [$method] $method ( $($ty),* ) [$($ret)?] }] [$($classes)*] [$($enums)*] $($rest)*);
    };
    // A class: its type, and the associated functions that hang in it.
    (@munch $name:literal [$($acc:tt)*] [$($classes:tt)*] [$($enums:ident)*]
        class $class:ident {
            $( fn $method:ident ( $($ty:ty),* $(,)? ) $(-> $ret:ty)? ; )*
        }
        $($rest:tt)*
    ) => {
        $crate::plugin!(@munch $name [$($acc)* $( { $class [$class :: $method] $method ( $($ty),* ) [$($ret)?] } )*] [$($classes)* $class] [$($enums)*] $($rest)*);
    };
    // An enum declared separately, e.g. by a plugin's mapping macro.
    (@munch $name:literal [$($acc:tt)*] [$($classes:tt)*] [$($enums:ident)*]
        enum $enum:ident;
        $($rest:tt)*
    ) => {
        $crate::plugin!(@munch $name [$($acc)*] [$($classes)*] [$($enums)* $enum] $($rest)*);
    };
    // Enum declarations generate Rust enums; Enum<T> is their C ABI carrier.
    (@munch $name:literal [$($acc:tt)*] [$($classes:tt)*] [$($enums:ident)*]
        enum $enum:ident { $( $variant:ident $( ( $( $field:ident : $ft:ty ),* $(,)? ) )? ; )* }
        $($rest:tt)*
    ) => {
        $crate::plugin_enum!($name, $enum, $( $variant $( ( $( $field : $ft ),* ) )? ; )*);
        $crate::plugin!(@munch $name [$($acc)*] [$($classes)*] [$($enums)* $enum] $($rest)*);
    };
    // Everything gathered: the checks, the tables, the entry.
    (@munch $name:literal [$( { $class:tt [$($path:tt)*] $method:ident ( $($ty:ty),* ) [$($ret:ty)?] } )*] [$($declared:ident)*] [$($enums:ident)*]) => {
        $(
            impl $crate::PluginClass for $declared {
                const NAME: &'static str = stringify!($declared);
                const TYPE_NAME: &'static str = concat!($name, ".", stringify!($declared));
            }
        )*

        $(
            // The item is what the declaration says, or this does not compile.
            const _: extern "C" fn($($ty),*) $(-> $ret)? = $($path)*;
        )*

        static __CARIBOU_CLASSES: [$crate::ClassDesc; $crate::plugin!(@count $($declared)*)] = [
            $(
                $crate::ClassDesc {
                    name: $crate::Str::new(stringify!($declared)),
                    drop: Some($crate::drop_boxed::<$declared>),
                }
            ),*
        ];

        static __CARIBOU_SYMBOLS: [$crate::SymbolDesc; $crate::plugin!(@count $($method)*)] = [
            $(
                $crate::SymbolDesc {
                    class: $crate::Str::new($crate::plugin!(@class $class)),
                    method: $crate::Str::new(stringify!($method)),
                    func: $($path)* as *const ::core::ffi::c_void,
                    flags: $crate::plugin!(@flags $class; $($ty),*),
                    param_count: $crate::plugin!(@count $($ty)*) as u8,
                    ret: $crate::plugin!(@tag $($ret)?),
                    params: $crate::padded(&[ $( <$ty as $crate::Param>::TAG ),* ]),
                    ret_enum: <$crate::plugin!(@ret_type $($ret)?) as $crate::Returned>::ENUM,
                    param_enums: $crate::data::padded_enums(&[$(<$ty as $crate::Param>::ENUM),*]),
                    future_ret: <$crate::plugin!(@ret_type $($ret)?) as $crate::Returned>::FUTURE_TAG,
                    future_ret_class: $crate::class_index(&__CARIBOU_CLASSES, <$crate::plugin!(@ret_type $($ret)?) as $crate::Returned>::FUTURE_CLASS),
                    future_ret_enum: <$crate::plugin!(@ret_type $($ret)?) as $crate::Returned>::FUTURE_ENUM,
                    ret_class: $crate::class_index(&__CARIBOU_CLASSES, $crate::plugin!(@ret_class $($ret)?)),
                    param_classes: $crate::padded_classes(&[
                        $( $crate::class_index(&__CARIBOU_CLASSES, <$ty as $crate::Param>::CLASS) ),*
                    ]),
                }
            ),*
        ];

        static __CARIBOU_INFO: $crate::PluginInfo = $crate::PluginInfo {
            abi_version: $crate::ABI_VERSION,
            name: $crate::Str::new($name),
            symbols: __CARIBOU_SYMBOLS.as_ptr(),
            symbol_count: __CARIBOU_SYMBOLS.len(),
            classes: __CARIBOU_CLASSES.as_ptr(),
            class_count: __CARIBOU_CLASSES.len(),
            enums: &[$(<$enums as $crate::PluginEnum>::DESC as *const $crate::EnumDesc),*] as *const _,
            enum_count: $crate::plugin!(@count $($enums)*),
        };

        #[unsafe(no_mangle)]
        pub extern "C" fn caribou_abi_version() -> u32 {
            $crate::ABI_VERSION
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn caribou_plugin_entry(host: *const $crate::host::Host) -> *const $crate::PluginInfo {
            $crate::host::install(host);
            &__CARIBOU_INFO
        }
    };
    (@class "") => { "" };
    (@class $class:ident) => { stringify!($class) };
    (@ret_type) => { () };
    (@ret_type $ret:ty) => { $ret };
    (@tag) => { <() as $crate::Returned>::TAG };
    (@tag $ret:ty) => { <$ret as $crate::Returned>::TAG };
    (@ret_class) => { None };
    (@ret_class $ret:ty) => { <$ret as $crate::Returned>::CLASS };
    // An instance method takes its own class as its first parameter. Another
    // class there is an ordinary parameter of a static or constructor.
    (@flags ""; $($ty:ty),*) => { $crate::sym::STATIC };
    (@flags $class:ident;) => { $crate::sym::STATIC };
    (@flags $class:ident; $first:ty $(, $ty:ty)*) => {
        if $crate::is_class(<$first as $crate::Param>::CLASS, stringify!($class)) {
            0
        } else {
            $crate::sym::STATIC
        }
    };
    (@count $($x:tt)*) => { <[()]>::len(&[ $( $crate::plugin!(@unit $x) ),* ]) };
    (@unit $x:tt) => { () };
}

// ---------------------------------------------------------------------------
// Tests. Every number comes from hl.h; changing one is an ABI break.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::hl::*;
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn hashlink_sizes_match_the_header() {
        assert_eq!(size_of::<hl_type>(), 32);
        assert_eq!(size_of::<vdynamic>(), 16);
        assert_eq!(size_of::<vobj>(), 8);
        assert_eq!(size_of::<vvirtual>(), 24);
        assert_eq!(size_of::<varray>(), 24);
        assert_eq!(size_of::<vclosure>(), 32);
        assert_eq!(size_of::<vclosure_wrapper>(), 40);
        assert_eq!(size_of::<venum>(), 16);
        assert_eq!(size_of::<vstring>(), 24);
        assert_eq!(size_of::<vdynobj>(), 56);
        assert_eq!(size_of::<hl_runtime_obj>(), 120);
        assert_eq!(size_of::<hl_runtime_binding>(), 24);
        assert_eq!(size_of::<hl_field_lookup>(), 16);
        assert_eq!(size_of::<hl_type_fun>(), 80);
        assert_eq!(size_of::<hl_type_obj>(), 80);
        assert_eq!(size_of::<hl_type_virtual>(), 32);
        assert_eq!(size_of::<hl_type_enum>(), 32);
        assert_eq!(size_of::<hl_enum_construct>(), 40);
        assert_eq!(size_of::<hl_obj_field>(), 24);
        assert_eq!(size_of::<hl_obj_proto>(), 24);
        assert_eq!(size_of::<hl_module_context>(), 24);
    }

    #[test]
    fn rooted_future_can_cross_executor_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Rooted<Future>>();
        assert_eq!(size_of::<Future>(), size_of::<usize>());
    }

    #[test]
    fn load_bearing_offsets_match_the_header() {
        assert_eq!(offset_of!(hl_type, kind), 0);
        assert_eq!(offset_of!(hl_type, detail), 8);
        assert_eq!(offset_of!(hl_type, vobj_proto), 16);
        assert_eq!(offset_of!(hl_type, mark_bits), 24);
        assert_eq!(offset_of!(vdynamic, v), 8);
        assert_eq!(offset_of!(varray, size), 16);
        assert_eq!(offset_of!(vclosure, fun), 8);
        assert_eq!(offset_of!(vclosure, hasValue), 16);
        assert_eq!(offset_of!(vclosure, stackCount), 20);
        assert_eq!(offset_of!(vclosure, value), 24);
        assert_eq!(offset_of!(hl_runtime_obj, hasPtr), 28);
        assert_eq!(offset_of!(hl_runtime_obj, pad_size), 29);
        assert_eq!(offset_of!(hl_runtime_obj, largest_field), 30);
        assert_eq!(offset_of!(hl_runtime_obj, methods), 32);
        assert_eq!(offset_of!(hl_runtime_obj, toStringFun), 64);
        assert_eq!(offset_of!(hl_runtime_obj, nlookup), 96);
        assert_eq!(offset_of!(hl_runtime_obj, lookup), 104);
        assert_eq!(offset_of!(hl_type_obj, name), 16);
        assert_eq!(offset_of!(hl_type_obj, rt), 72);
        assert_eq!(offset_of!(hl_type_fun, closure_type), 32);
        assert_eq!(offset_of!(hl_type_fun, closure), 48);
        assert_eq!(offset_of!(vdynobj, nfields), 32);
        assert_eq!(offset_of!(vdynobj, virtuals), 48);
    }

    #[test]
    fn type_sizes_match_hl_type_size() {
        let expected = [0usize, 1, 2, 4, 8, 4, 8, 1];
        for (kind, size) in expected.iter().enumerate() {
            assert_eq!(type_size(kind as hl_type_kind), *size);
        }
        for kind in HBYTES..HLAST {
            assert_eq!(type_size(kind), HL_WSIZE);
            assert!(is_ptr(kind));
        }
        assert!(!is_ptr(HBOOL));
    }

    #[test]
    fn value_is_one_word() {
        assert_eq!(size_of::<Value>(), 8);
    }

    #[test]
    fn numbers_round_trip_and_never_look_like_tags() {
        for n in [
            0.0,
            -0.0,
            1.5,
            -2.0e300,
            f64::MIN_POSITIVE,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let v = Value::number(n);
            assert!(v.is_number());
            assert!(!v.is_object() && !v.is_int() && !v.is_null() && !v.is_bool());
            assert_eq!(v.as_number().unwrap().to_bits(), n.to_bits());
        }
        let nan = Value::number(f64::NAN);
        assert!(nan.is_number());
        assert!(nan.as_number().unwrap().is_nan());
        let nasty = Value::number(f64::from_bits(0xFFFC_0000_0000_0007));
        assert!(nasty.is_number());
        assert!(!nasty.is_object());
    }

    #[test]
    fn singletons_and_ints_round_trip() {
        assert!(Value::null().is_null());
        assert!(!Value::null().is_truthy());
        assert!(!Value::bool(false).is_truthy());
        assert!(Value::bool(true).is_truthy());
        assert!(Value::number(0.0).is_truthy());
        assert_eq!(Value::bool(true).as_bool(), Some(true));
        assert_eq!(Value::bool(false).as_bool(), Some(false));
        assert!(Value::undefined().is_undefined());
        for n in [0, 1, -1, i32::MAX, i32::MIN, 123456789] {
            let v = Value::int(n);
            assert!(v.is_int());
            assert!(!v.is_number() && !v.is_object() && !v.is_null() && !v.is_bool());
            assert_eq!(v.as_int(), Some(n));
        }
        assert_eq!(Value::int(-1).to_bits(), Value::TAG_I32 | 0xFFFF_FFFF);
    }

    #[test]
    fn objects_round_trip_including_bit_47() {
        for addr in [
            0x1000usize,
            0x0000_7FFF_FFFF_FFF0,
            0x0000_8000_0000_0010,
            0x0000_FFFF_FFFF_FFF8,
        ] {
            let v = Value::object(addr as *const c_void);
            assert!(v.is_object());
            assert!(!v.is_number() && !v.is_int() && !v.is_null());
            assert_eq!(v.as_object().unwrap() as usize, addr);
        }
        assert_eq!(
            Value::object(core::ptr::null()).as_object(),
            Some(core::ptr::null_mut())
        );
    }

    #[test]
    fn alloc_kinds_come_from_the_low_bits() {
        use mem::*;
        assert_eq!(AllocKind::from_flags(KIND_DYNAMIC | ZERO), AllocKind::Typed);
        assert_eq!(
            AllocKind::from_flags(KIND_RAW | ALIGN_DOUBLE),
            AllocKind::Raw
        );
        assert_eq!(AllocKind::from_flags(KIND_NOPTR), AllocKind::NoPtr);
        assert_eq!(
            AllocKind::from_flags(KIND_FINALIZER | ZERO),
            AllocKind::Finalizer
        );
        assert_eq!(
            AllocKind::from_flags(KIND_DYNAMIC | TRACED),
            AllocKind::Typed
        );
        assert_eq!(TRACED & (KIND_MASK | ALIGN_DOUBLE | ZERO), 0);
    }

    #[test]
    fn descriptor_table_is_plain_data() {
        static SYMS: [SymbolDesc; 1] = [SymbolDesc {
            class: Str::new("Texture"),
            method: Str::new("width"),
            func: core::ptr::null(),
            flags: 0,
            param_count: 0,
            ret: TypeTag::I32,
            params: [TypeTag::VOID; MAX_PARAMS],
            ret_class: NO_CLASS,
            param_classes: [NO_CLASS; MAX_PARAMS],
            ret_enum: core::ptr::null(),
            param_enums: [core::ptr::null(); MAX_PARAMS],
            future_ret: TypeTag::VOID,
            future_ret_class: NO_CLASS,
            future_ret_enum: core::ptr::null(),
        }];
        static INFO: PluginInfo = PluginInfo {
            abi_version: ABI_VERSION,
            name: Str::new("gpu"),
            symbols: SYMS.as_ptr(),
            symbol_count: SYMS.len(),
            classes: core::ptr::null(),
            class_count: 0,
            enums: core::ptr::null(),
            enum_count: 0,
        };
        assert_eq!(INFO.abi_version, ABI_VERSION);
        assert_eq!(unsafe { INFO.name.as_str() }, "gpu");
        assert_eq!(unsafe { SYMS[0].class.as_str() }, "Texture");
        assert_eq!(SYMS[0].ret.kind(), HI32);
        assert_eq!(unsafe { Str::EMPTY.as_str() }, "");
    }
}
