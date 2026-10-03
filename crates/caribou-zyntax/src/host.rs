//! The world's modules as the typed host modules a Zyntax frontend checks
//! a program's imports and calls against (`zyntax_embed::host`): a
//! module's classes and functions from its interface in the registry. The
//! calls themselves go through the foreign-object protocol (`foreign`).

use caribou::registry::{self, ClassIface, Interface, MethodIface, MethodKind, TypeRef};
use zyntax_embed::host::{HostClass, HostField, HostMethod, HostModule, HostType};

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

fn host_method(method: &MethodIface) -> HostMethod {
    HostMethod {
        name: method.name.clone(),
        key: host_key(&method.name, Some(method.target)),
        params: method.params.iter().map(host_type).collect(),
        ret: host_type(&method.ret),
        is_static: method.is_static,
    }
}

fn host_class(class: &ClassIface) -> HostClass {
    let mut fields: Vec<HostField> = class
        .fields
        .iter()
        .map(|field| HostField {
            name: field.name.clone(),
            key: host_key(&field.name, None),
            ty: host_type(&field.ty),
            is_static: false,
        })
        .chain(class.statics.iter().map(|field| HostField {
            name: field.name.clone(),
            key: host_key(&field.name, None),
            ty: host_type(&field.ty),
            is_static: true,
        }))
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
            .map(host_method)
            .collect(),
        constructor: class.ctor.as_ref().map(|constructor| {
            let mut method = host_method(constructor);
            method.name = class.name.clone();
            method
        }),
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
        functions: iface.functions.iter().map(host_method).collect(),
    })
}
