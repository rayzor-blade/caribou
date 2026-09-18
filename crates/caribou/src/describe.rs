//! A module's interface as data: what a build step reads to declare another
//! language's classes before anything runs.
//!
//! [`ModuleDesc`] is the [`Interface`](crate::registry::Interface) without
//! its callables: the same classes, members, kinds and types, plus the
//! parameter names a source has and a running program does not. An adapter
//! produces one from source (`caribou-wren --describe`) and, through
//! [`ModuleDesc::of`], from what it published, so the two agree by
//! construction. With the `serde` feature it reads and writes as JSON.

use crate::registry::{Interface, MethodKind, TypeRef};

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ModuleDesc {
    /// The language's name, as registered.
    pub lang: String,
    pub module: String,
    pub classes: Vec<ClassDesc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassDesc {
    pub name: String,
    /// The type's name in its own language: what an instance reports.
    pub type_name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub superclass: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fields: Vec<FieldDesc>,
    /// Fields of the class itself.
    #[cfg_attr(feature = "serde", serde(default))]
    pub statics: Vec<FieldDesc>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub members: Vec<MemberDesc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldDesc {
    pub name: String,
    pub ty: TypeRef,
}

/// What a member is to its class. A `Constructor` is the one `new`; a
/// `Factory` is any other constructor, a static returning the class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum MemberKind {
    Method,
    Static,
    Getter,
    Setter,
    Constructor,
    Factory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MemberDesc {
    pub name: String,
    pub kind: MemberKind,
    /// The member in its language's own spelling, `hit(_)` or `hp=(_)`
    /// for Wren: what the bridge is asked for.
    pub signature: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub params: Vec<ParamDesc>,
    #[cfg_attr(feature = "serde", serde(default = "TypeRef::dyn_"))]
    pub ret: TypeRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ParamDesc {
    pub name: String,
    pub ty: TypeRef,
}

impl TypeRef {
    /// The default for an undeclared type.
    pub fn dyn_() -> TypeRef {
        TypeRef::Dyn
    }
}

impl ModuleDesc {
    /// The description of a published interface. A program has no
    /// parameter names, so they are `a0`, `a1`, and so on.
    pub fn of(iface: &Interface, lang: &str) -> ModuleDesc {
        let members = |class: &crate::registry::ClassIface| {
            let mut out = Vec::new();
            for m in class.ctor.iter().chain(&class.methods) {
                let is_ctor = class.ctor.as_ref().is_some_and(|c| std::ptr::eq(c, m));
                let kind = if is_ctor {
                    MemberKind::Constructor
                } else if m.is_static {
                    if m.ret == TypeRef::Object(class.type_name.clone()) {
                        MemberKind::Factory
                    } else {
                        MemberKind::Static
                    }
                } else {
                    m.kind().member()
                };
                out.push(MemberDesc {
                    name: m.name.clone(),
                    kind,
                    signature: signature_of(m),
                    params: m
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| ParamDesc {
                            name: format!("a{i}"),
                            ty: ty.clone(),
                        })
                        .collect(),
                    ret: m.ret.clone(),
                });
            }
            out
        };
        ModuleDesc {
            lang: lang.to_owned(),
            module: iface.module.clone(),
            classes: iface
                .classes
                .iter()
                .map(|c| ClassDesc {
                    name: c.name.clone(),
                    type_name: c.type_name.clone(),
                    superclass: c.superclass.clone(),
                    fields: c.fields.iter().map(field).collect(),
                    statics: c.statics.iter().map(field).collect(),
                    members: members(c),
                })
                .collect(),
        }
    }
}

fn field(f: &crate::registry::FieldIface) -> FieldDesc {
    FieldDesc {
        name: f.name.clone(),
        ty: f.ty.clone(),
    }
}

/// A member's spelling: the signature its callable carries when it has
/// one, else its name with its arity in the same spelling, `dot(_,_)`,
/// which is what the bridge is asked for.
fn signature_of(m: &crate::registry::MethodIface) -> String {
    match m.target {
        crate::protocol::Callable::WrenMethod { signature, .. } => signature.name().to_owned(),
        _ => format!("{}({})", m.name, vec!["_"; m.params.len()].join(",")),
    }
}

impl MethodKind {
    /// The member kind a method kind is, for a non-static member.
    pub fn member(self) -> MemberKind {
        match self {
            MethodKind::Method => MemberKind::Method,
            MethodKind::Getter => MemberKind::Getter,
            MethodKind::Setter => MemberKind::Setter,
        }
    }
}
