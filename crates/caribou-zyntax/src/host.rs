//! The world's modules as the typed host modules a Zyntax frontend checks
//! a program's imports and calls against (`zyntax_embed::host`): a
//! module's classes and functions from its interface in the registry. A
//! member the registry gives native code or storage for is called or
//! accessed in place; every other call goes through the foreign-object
//! protocol (`foreign`). A program holds an object as its core word.

use caribou::registry::{
    self, ClassIface, FieldIface, Interface, MethodIface, MethodKind, NativeFn, NativePass,
    NativeSlot, TypeRef,
};
use zyntax_embed::host::{
    self as zh, HostClass, HostField, HostMethod, HostModule, HostType, NativeBinding, NativeField,
};

use crate::foreign::host_key;

fn host_type(ty: &TypeRef) -> HostType {
    match ty {
        TypeRef::Void => HostType::Void,
        TypeRef::Bool => HostType::Bool,
        TypeRef::Int | TypeRef::Int64 => HostType::Int,
        TypeRef::Float => HostType::Float,
        TypeRef::Str => HostType::Str,
        TypeRef::Buffer => HostType::Bytes,
        TypeRef::Object(name) => HostType::Object(name.clone()),
        TypeRef::Function { params, ret } => HostType::Function {
            params: params.iter().map(host_type).collect(),
            ret: Box::new(host_type(ret)),
        },
        TypeRef::Enum(_)
        | TypeRef::Optional(_)
        | TypeRef::Future(_)
        | TypeRef::Array(_)
        | TypeRef::Dyn
        | TypeRef::Fun
        | TypeRef::Tuple(_) => HostType::Dynamic,
    }
}

fn native_pass(pass: NativePass) -> zh::NativePass {
    match pass {
        NativePass::Word => zh::NativePass::Word,
        NativePass::Indirect(k) => zh::NativePass::Indirect(k),
    }
}

/// How a bound call carries `ty`; `None` where it cannot: a core string
/// parameter, where the program has its own string's bytes.
fn native_type(ty: &registry::NativeType, param: bool) -> Option<zh::NativeType> {
    use registry::NativeType as N;
    Some(match ty {
        N::Void => zh::NativeType::Void,
        N::Bool => zh::NativeType::Bool,
        N::U8 => zh::NativeType::U8,
        N::U16 => zh::NativeType::U16,
        N::I32 => zh::NativeType::I32,
        N::I64 => zh::NativeType::I64,
        N::F32 => zh::NativeType::F32,
        N::F64 => zh::NativeType::F64,
        N::Text if param => return None,
        N::Text => zh::NativeType::Str,
        N::Object { type_name, pass } => zh::NativeType::Object {
            type_name: type_name.clone(),
            pass: native_pass(*pass),
        },
    })
}

/// `native` as the binding of the member `symbol`.
fn binding(symbol: String, native: &NativeFn) -> Option<NativeBinding> {
    Some(NativeBinding {
        symbol,
        address: native.func as usize,
        receiver: native.receiver.map(native_pass),
        params: native
            .params
            .iter()
            .map(|ty| native_type(ty, true))
            .collect::<Option<_>>()?,
        ret: native_type(&native.ret, false)?,
        may_raise: native.may_raise,
    })
}

fn native_field(slot: &NativeSlot) -> Option<NativeField> {
    Some(NativeField {
        offset: slot.offset,
        ty: native_type(&slot.ty, false)?,
        pass: native_pass(slot.pass),
    })
}

/// `method` of `owner`: a class's type name, or a module's name.
fn host_method(owner: &str, method: &MethodIface) -> HostMethod {
    HostMethod {
        name: method.name.clone(),
        key: host_key(&method.name, Some(method.target)),
        params: method.params.iter().map(host_type).collect(),
        ret: host_type(&method.ret),
        is_static: method.is_static,
        native: method
            .native
            .as_ref()
            .and_then(|native| binding(format!("{owner}.{}", method.name), native)),
    }
}

fn host_field(field: &FieldIface, is_static: bool) -> HostField {
    HostField {
        name: field.name.clone(),
        key: host_key(&field.name, None),
        ty: host_type(&field.ty),
        is_static,
        writable: field.native.as_ref().is_none_or(|slot| slot.writable),
        native: field.native.as_ref().and_then(native_field),
    }
}

fn host_class(class: &ClassIface) -> HostClass {
    let owner = class.type_name.as_str();
    let mut fields: Vec<HostField> = class
        .fields
        .iter()
        .map(|field| host_field(field, false))
        .chain(class.statics.iter().map(|field| host_field(field, true)))
        .collect();
    for method in &class.methods {
        if method.kind() == MethodKind::Getter
            && !fields
                .iter()
                .any(|field| field.is_static == method.is_static && field.name == method.name)
        {
            fields.push(HostField {
                name: method.name.clone(),
                key: host_key(&method.name, None),
                ty: host_type(&method.ret),
                is_static: method.is_static,
                // A store the language refuses raises through the protocol.
                writable: true,
                native: None,
            });
        }
    }
    HostClass {
        name: class.name.clone(),
        type_name: class.type_name.clone(),
        fields,
        methods: class
            .methods
            .iter()
            .filter(|method| method.kind() == MethodKind::Method)
            .map(|method| host_method(owner, method))
            .collect(),
        constructor: class.ctor.as_ref().map(|constructor| {
            let mut method = host_method(owner, constructor);
            method.name = class.name.clone();
            method
        }),
        word: true,
    }
}

/// The world's module `namespace.module` as a host module, loading it
/// if it is not yet; `None` when the world has none by that name.
pub fn module(name: &str) -> Option<HostModule> {
    let (namespace, module) = name.split_once('.')?;
    let iface: std::sync::Arc<Interface> = registry::lookup_or_load(namespace, module).ok()??;
    Some(HostModule {
        name: name.to_owned(),
        classes: iface.classes.iter().map(host_class).collect(),
        functions: iface
            .functions
            .iter()
            .map(|function| host_method(name, function))
            .collect(),
    })
}

/// What a bound call's site reads after a call that may raise: the
/// address of the thread's pending flag.
extern "C" fn pending_flag() -> i64 {
    caribou::bridge::pending_flag() as i64
}

/// The current task's pending error, handed to the program as a foreign
/// error, which it raises as it raises any.
extern "C" fn raise_pending() {
    if let Some(error) = caribou::bridge::take_pending() {
        crate::foreign::report(error);
    }
}

/// A bound call's string result, a core string, as a Zyntax string.
extern "C" fn text_to_string(core: i64) -> *mut std::ffi::c_void {
    let value = caribou_abi::Value::object(core as *const std::ffi::c_void);
    crate::object::zyntax_string(unsafe { caribou::error::Str::text(value) }.unwrap_or_default())
}

/// Make the entries a bound call's site reaches the core through known
/// to every runtime, once per process.
pub fn install() {
    use zyntax_compiler::late_symbols::register;
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        register("$Host$pending_flag", pending_flag as *const u8);
        register("$Host$raise_pending", raise_pending as *const u8);
        register("$Host$text_to_string", text_to_string as *const u8);
    });
}
