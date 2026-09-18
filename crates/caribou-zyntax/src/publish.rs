//! A module's interface from what it declares. The typed AST is the
//! source of truth for names and types: a function's parameters and
//! result as Zyntax types (`Type`), a struct or class with its fields,
//! its methods, static or on an instance, and its constructors. The HIR
//! is the source of truth for how each function is called: its symbol,
//! and the machine type of each value at the C boundary, which the typed
//! signature disambiguates (a `String` is a pointer in the HIR, so is an
//! object). What no language can call yet still publishes with its
//! types; the call is what fails, naming the type.

use caribou::native;
use caribou::protocol::Callable;
use caribou::registry::{ClassIface, FieldIface, Interface, MethodIface, TypeRef};
use caribou_abi::LangId;
use caribou_abi::hl::{self, hl_type};
use zyntax_compiler::hir::{HirFunction, HirType};
use zyntax_embed::HirModule;
use std::collections::HashMap;

use zyntax_typed_ast::type_registry::{PrimitiveType, Type, TypeId, TypeRegistry};
use zyntax_typed_ast::{InternedString, TypedDeclaration, TypedProgram};

/// A function as published: the name a language calls it by, the
/// runtime's symbol, its typed parameters and result, and whether it
/// takes a receiver.
pub struct Function {
    pub name: String,
    pub symbol: String,
    pub is_static: bool,
    /// Every parameter's type, the receiver's first for a method.
    pub params: Vec<TypeRef>,
    pub ret: TypeRef,
    /// The C-boundary kind of each parameter and of the result, from the
    /// HIR: what the dispatcher passes.
    pub kinds: Vec<hl::hl_type_kind>,
    pub ret_kind: hl::hl_type_kind,
}

/// A struct or class as published.
pub struct Class {
    pub name: String,
    pub fields: Vec<(String, TypeRef)>,
    pub methods: Vec<Function>,
    pub ctor: Option<Function>,
}

/// What a module declares: its classes, and the functions outside any.
pub struct Declared {
    pub classes: Vec<Class>,
    pub functions: Vec<Function>,
}

/// The kind a value of a Zyntax type has at the C boundary, given what
/// the HIR made of it. An object is `HOBJ`, an array `HARRAY`, a
/// function `HFUN`: kinds the dispatcher does not pass yet.
fn kind_of(ty: &Type, hir: &HirType) -> hl::hl_type_kind {
    match hir {
        HirType::Void => hl::HVOID,
        HirType::Bool => hl::HBOOL,
        HirType::U8 => hl::HUI8,
        HirType::U16 => hl::HUI16,
        HirType::I8 | HirType::I16 | HirType::I32 | HirType::U32 => hl::HI32,
        HirType::I64 | HirType::U64 | HirType::ISize | HirType::USize
            if !matches!(ty, Type::Named { .. } | Type::Unresolved(_)) =>
        {
            hl::HI64
        }
        HirType::F32 => hl::HF32,
        HirType::F64 => hl::HF64,
        HirType::Ptr(_) if matches!(ty, Type::Primitive(PrimitiveType::String)) => hl::HBYTES,
        _ => match ty {
            Type::Array { .. } => hl::HARRAY,
            Type::Function { .. } => hl::HFUN,
            _ => hl::HOBJ,
        },
    }
}

/// The program's types as published: its registry, and the ids of the
/// classes it declares, which the lowering registers on its own copy of
/// the program (a declaration's node type names its id).
pub struct Types<'a> {
    registry: &'a TypeRegistry,
    declared: HashMap<TypeId, String>,
}

impl<'a> Types<'a> {
    fn of(program: &'a TypedProgram) -> Types<'a> {
        let mut declared = HashMap::new();
        for node in &program.declarations {
            if let TypedDeclaration::Class(c) = &node.node
                && let Type::Named { id, .. } = &node.ty
            {
                declared.insert(*id, name_of(c.name));
            }
        }
        Types {
            registry: &program.type_registry,
            declared,
        }
    }

    /// The name of a type that is a declared type: resolved through the
    /// registry, declared by the program, or as the parser wrote it when
    /// nothing has resolved it yet.
    fn named(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::Named { id, .. } => self
                .registry
                .get_type_by_id(*id)
                .map(|def| name_of(def.name))
                .or_else(|| self.declared.get(id).cloned()),
            Type::Unresolved(name) => Some(name_of(*name)),
            _ => None,
        }
    }
}

/// The registry's type for a Zyntax type. An object is named as the
/// language names it, `zynml.Point`.
pub fn type_ref(ty: &Type, types: &Types<'_>, lang: &str) -> TypeRef {
    if let Some(name) = types.named(ty) {
        return TypeRef::Object(format!("{lang}.{name}"));
    }
    match ty {
        Type::Primitive(p) => match p {
            PrimitiveType::Unit => TypeRef::Void,
            PrimitiveType::Bool => TypeRef::Bool,
            PrimitiveType::F32 | PrimitiveType::F64 => TypeRef::Float,
            PrimitiveType::String | PrimitiveType::Char => TypeRef::Str,
            _ => TypeRef::Int,
        },
        Type::Array { element_type, .. } => {
            TypeRef::Array(Box::new(type_ref(element_type, types, lang)))
        }
        Type::Optional(inner) | Type::Nullable(inner) | Type::NonNull(inner) => {
            type_ref(inner, types, lang)
        }
        Type::Function { .. } => TypeRef::Fun,
        _ => TypeRef::Dyn,
    }
}

fn name_of(name: InternedString) -> String {
    name.resolve_global().unwrap_or_default()
}

/// The HIR function for a symbol, by name.
fn hir_function<'a>(hir: &'a HirModule, symbol: &str) -> Option<&'a HirFunction> {
    hir.functions
        .values()
        .find(|f| f.name.resolve_global().as_deref() == Some(symbol))
}

/// A function or method as the typed AST declares it.
struct Signature<'a> {
    name: String,
    symbol: String,
    is_static: bool,
    params: Vec<&'a Type>,
    ret: &'a Type,
}

/// A declared function or method against its HIR: `None` when the HIR
/// has no function of that symbol (one the lowering left out).
fn function(
    sig: Signature<'_>,
    hir: &HirModule,
    types: &Types<'_>,
    lang: &str,
) -> Option<Function> {
    let f = hir_function(hir, &sig.symbol)?;
    let hir_params = &f.signature.params;
    let mut kinds = Vec::with_capacity(sig.params.len());
    let mut refs = Vec::with_capacity(sig.params.len());
    for (i, ty) in sig.params.iter().enumerate() {
        let hir_ty = hir_params.get(i).map(|p| &p.ty).unwrap_or(&HirType::Void);
        kinds.push(kind_of(ty, hir_ty));
        refs.push(type_ref(ty, types, lang));
    }
    let hir_ret = f.signature.returns.first().unwrap_or(&HirType::Void);
    Some(Function {
        name: sig.name,
        symbol: sig.symbol,
        is_static: sig.is_static,
        params: refs,
        ret: type_ref(sig.ret, types, lang),
        kinds,
        ret_kind: kind_of(sig.ret, hir_ret),
    })
}

/// What `program` declares, against `hir`. A method's symbol is
/// `Type$method`, as the lowering names it; a struct's or class's own
/// methods and those an inherent `impl` adds are one list.
pub fn declared(program: &TypedProgram, hir: &HirModule, lang: &str) -> Declared {
    let types = &Types::of(program);
    let mut classes: Vec<Class> = Vec::new();
    let mut functions = Vec::new();
    let class_at = |classes: &mut Vec<Class>, name: String| -> usize {
        match classes.iter().position(|c| c.name == name) {
            Some(i) => i,
            None => {
                classes.push(Class {
                    name,
                    fields: Vec::new(),
                    methods: Vec::new(),
                    ctor: None,
                });
                classes.len() - 1
            }
        }
    };
    let method = |class: &str, m: &zyntax_typed_ast::TypedMethod| -> Option<Function> {
        let name = name_of(m.name);
        let sig = Signature {
            symbol: format!("{class}${name}"),
            is_static: !m.params.iter().any(|p| p.is_self),
            params: m.params.iter().map(|p| &p.ty).collect(),
            ret: &m.return_type,
            name,
        };
        function(sig, hir, types, lang)
    };
    for node in &program.declarations {
        match &node.node {
            TypedDeclaration::Function(f) => {
                let name = name_of(f.name);
                let sig = Signature {
                    symbol: name.clone(),
                    is_static: true,
                    params: f.params.iter().map(|p| &p.ty).collect(),
                    ret: &f.return_type,
                    name,
                };
                if let Some(function) = function(sig, hir, types, lang) {
                    functions.push(function);
                }
            }
            TypedDeclaration::Class(c) => {
                let name = name_of(c.name);
                let at = class_at(&mut classes, name.clone());
                for field in &c.fields {
                    classes[at]
                        .fields
                        .push((name_of(field.name), type_ref(&field.ty, types, lang)));
                }
                for m in &c.methods {
                    if let Some(function) = method(&name, m) {
                        classes[at].methods.push(function);
                    }
                }
            }
            TypedDeclaration::Impl(imp) => {
                let Some(name) = types.named(&imp.for_type) else {
                    continue;
                };
                let at = class_at(&mut classes, name.clone());
                for m in &imp.methods {
                    if let Some(function) = method(&name, m) {
                        classes[at].methods.push(function);
                    }
                }
            }
            _ => {}
        }
    }
    // A static `new` returning its own class is the constructor.
    for class in &mut classes {
        let own = format!("{lang}.{}", class.name);
        if let Some(i) = class
            .methods
            .iter()
            .position(|m| m.name == "new" && m.is_static && m.ret == TypeRef::Object(own.clone()))
        {
            class.ctor = Some(class.methods.remove(i));
        }
    }
    Declared { classes, functions }
}

/// The interface of a module: each class it declares, and, when it
/// declares functions outside any class, a class named after the module
/// (`scorer` → `Scorer`) whose statics they are, since a language
/// imports classes. `func_of` gives a symbol's compiled address.
pub fn interface(
    lang: LangId,
    lang_name: &str,
    module: &str,
    short: &str,
    declared: Declared,
    func_of: &dyn Fn(&str) -> Option<*const u8>,
) -> Interface {
    let method = |f: &Function| -> Option<MethodIface> {
        let func = func_of(&f.symbol)?;
        let params: Vec<*const hl_type> = f
            .kinds
            .iter()
            .map(|&k| native::kind_type(k) as *const hl_type)
            .collect();
        // The receiver is the target's first argument, not a parameter.
        let declared = if f.is_static { 0 } else { 1 };
        Some(MethodIface {
            name: f.name.clone(),
            is_static: f.is_static,
            params: f.params.iter().skip(declared).cloned().collect(),
            ret: f.ret.clone(),
            target: Callable::Typed {
                func: func as *const std::ffi::c_void,
                signature: native::signature(&params, native::kind_type(f.ret_kind)),
                lang,
            },
        })
    };
    let mut classes: Vec<ClassIface> = declared
        .classes
        .iter()
        .map(|c| ClassIface {
            name: c.name.clone(),
            type_name: format!("{lang_name}.{}", c.name),
            superclass: None,
            fields: c
                .fields
                .iter()
                .map(|(name, ty)| FieldIface {
                    name: name.clone(),
                    ty: ty.clone(),
                })
                .collect(),
            statics: Vec::new(),
            methods: c.methods.iter().filter_map(method).collect(),
            ctor: c.ctor.as_ref().and_then(method),
            class_object: caribou_abi::Value::null(),
        })
        .collect();
    if !declared.functions.is_empty() {
        let class = capitalised(short);
        classes.push(ClassIface {
            name: class.clone(),
            type_name: format!("{lang_name}.{class}"),
            superclass: None,
            fields: Vec::new(),
            statics: Vec::new(),
            methods: declared.functions.iter().filter_map(method).collect(),
            ctor: None,
            class_object: caribou_abi::Value::null(),
        });
    }
    Interface {
        lang,
        module: module.to_owned(),
        classes,
    }
}

fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
