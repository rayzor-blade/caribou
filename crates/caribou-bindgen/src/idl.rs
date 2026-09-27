//! A WebIDL file, parsed whole (`weedle2`) into what the generators read:
//! every type resolved through its typedefs, and each interface with its
//! partial definitions, mixins and inherited interface named.

use std::collections::BTreeMap;

use weedle::Definition;
use weedle::argument::Argument;
use weedle::interface::InterfaceMember;
use weedle::mixin::MixinMember;
use weedle::types::{
    FloatingPointType, IntegerType, NonAnyType, RecordKeyType, ReturnType, SingleType, Type,
    UnionMemberType,
};

/// A WebIDL type, typedefs resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Undefined,
    Boolean,
    Integer(Int),
    /// `true` for `double`.
    Float(bool),
    String,
    /// Bytes the caller holds: `BufferSource`, `ArrayBuffer`, a typed
    /// array, `AllowSharedBufferSource`.
    Bytes,
    Enum(String),
    Interface(String),
    Dictionary(String),
    Sequence(Box<Ty>),
    /// A record keyed by strings.
    Record(Box<Ty>),
    Nullable(Box<Ty>),
    /// The typedef that names it, when one does, and the alternatives.
    Union(Option<String>, Vec<Ty>),
    Promise(Box<Ty>),
    /// What the wire does not carry: `any`, `object`, callbacks, events.
    Opaque(String),
}

/// A WebIDL integer's width and sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Int {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub ty: Ty,
    pub required: bool,
}

#[derive(Clone, Debug)]
pub struct Dictionary {
    pub name: String,
    pub inherits: Option<String>,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
    pub variadic: bool,
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub name: String,
    pub args: Vec<Arg>,
    pub ret: Ty,
}

#[derive(Clone, Debug)]
pub struct Attribute {
    pub name: String,
    pub ty: Ty,
    pub readonly: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Interface {
    pub name: String,
    pub inherits: Option<String>,
    pub operations: Vec<Operation>,
    pub attributes: Vec<Attribute>,
    /// The element type of a `setlike`.
    pub setlike: Option<Ty>,
}

/// The whole file, in declaration order.
#[derive(Debug, Default)]
pub struct Model {
    pub enums: BTreeMap<String, Vec<String>>,
    pub dictionaries: Vec<Dictionary>,
    pub interfaces: Vec<Interface>,
}

impl Model {
    pub fn dictionary(&self, name: &str) -> Option<&Dictionary> {
        self.dictionaries.iter().find(|d| d.name == name)
    }

    pub fn interface(&self, name: &str) -> Option<&Interface> {
        self.interfaces.iter().find(|i| i.name == name)
    }

    /// A dictionary's members, its ancestors' first, as WebIDL orders them.
    pub fn dictionary_members(&self, name: &str) -> Vec<&Member> {
        let Some(d) = self.dictionary(name) else {
            return Vec::new();
        };
        let mut out = d
            .inherits
            .as_deref()
            .map(|parent| self.dictionary_members(parent))
            .unwrap_or_default();
        out.extend(d.members.iter());
        out
    }
}

/// weedle2 predates WebIDL's constants in a namespace. A namespace holding
/// only constants declares what an interface of the same constants does,
/// so it is read as one.
fn normalize(text: &str) -> String {
    text.lines()
        .map(|line| match line.strip_prefix("namespace ") {
            Some(rest) => format!("interface {rest}"),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse `text` whole.
pub fn parse(text: &str) -> Result<Model, String> {
    let text = normalize(text);
    let definitions = weedle::parse(&text).map_err(|e| format!("WebIDL: {e:?}"))?;

    // Names first: an identifier's kind decides its type.
    let mut kinds: BTreeMap<String, Kind> = BTreeMap::new();
    let mut typedefs: BTreeMap<String, &weedle::types::Type> = BTreeMap::new();
    for d in &definitions {
        match d {
            Definition::Enum(e) => {
                kinds.insert(e.identifier.0.to_owned(), Kind::Enum);
            }
            Definition::Dictionary(x) => {
                kinds.insert(x.identifier.0.to_owned(), Kind::Dictionary);
            }
            Definition::Interface(x) => {
                kinds.insert(x.identifier.0.to_owned(), Kind::Interface);
            }
            Definition::Callback(x) => {
                kinds.insert(x.identifier.0.to_owned(), Kind::Opaque);
            }
            Definition::CallbackInterface(x) => {
                kinds.insert(x.identifier.0.to_owned(), Kind::Opaque);
            }
            Definition::Typedef(t) => {
                typedefs.insert(t.identifier.0.to_owned(), &t.type_.type_);
            }
            _ => {}
        }
    }
    let r = Resolver { kinds: &kinds, typedefs: &typedefs };

    let mut model = Model::default();
    let mut mixins: BTreeMap<String, Interface> = BTreeMap::new();
    let mut includes: Vec<(String, String)> = Vec::new();
    for d in &definitions {
        match d {
            Definition::Enum(e) => {
                let values = e.values.body.list.iter().map(|v| v.value.0.to_owned()).collect();
                model.enums.insert(e.identifier.0.to_owned(), values);
            }
            Definition::Dictionary(x) => model.dictionaries.push(Dictionary {
                name: x.identifier.0.to_owned(),
                inherits: x.inheritance.map(|i| i.identifier.0.to_owned()),
                members: x
                    .members
                    .body
                    .iter()
                    .map(|m| Member {
                        name: m.identifier.0.to_owned(),
                        ty: r.ty(&m.type_),
                        required: m.required.is_some(),
                    })
                    .collect(),
            }),
            Definition::PartialDictionary(x) => {
                let members = x.members.body.iter().map(|m| Member {
                    name: m.identifier.0.to_owned(),
                    ty: r.ty(&m.type_),
                    required: m.required.is_some(),
                });
                match model.dictionaries.iter_mut().find(|d| d.name == x.identifier.0) {
                    Some(d) => d.members.extend(members),
                    None => return Err(format!("partial dictionary {} before its dictionary", x.identifier.0)),
                }
            }
            Definition::Interface(x) => {
                let mut i = Interface {
                    name: x.identifier.0.to_owned(),
                    inherits: x.inheritance.map(|i| i.identifier.0.to_owned()),
                    ..Interface::default()
                };
                r.interface_members(&mut i, &x.members.body);
                model.interfaces.push(i);
            }
            Definition::PartialInterface(x) => {
                let name = x.identifier.0;
                let Some(i) = model.interfaces.iter_mut().find(|i| i.name == name) else {
                    return Err(format!("partial interface {name} before its interface"));
                };
                r.interface_members(i, &x.members.body);
            }
            Definition::InterfaceMixin(x) => {
                let mut i = Interface { name: x.identifier.0.to_owned(), ..Interface::default() };
                r.mixin_members(&mut i, &x.members.body);
                mixins.insert(i.name.clone(), i);
            }
            Definition::PartialInterfaceMixin(x) => {
                let i = mixins
                    .entry(x.identifier.0.to_owned())
                    .or_insert_with(|| Interface { name: x.identifier.0.to_owned(), ..Interface::default() });
                r.mixin_members(i, &x.members.body);
            }
            Definition::IncludesStatement(x) => {
                includes.push((x.lhs_identifier.0.to_owned(), x.rhs_identifier.0.to_owned()));
            }
            _ => {}
        }
    }
    for (target, mixin) in includes {
        let Some(m) = mixins.get(&mixin) else {
            return Err(format!("{target} includes {mixin}, which is not a mixin"));
        };
        // A mixin a navigator or worker global includes adds nothing the
        // wire reaches.
        if let Some(i) = model.interfaces.iter_mut().find(|i| i.name == target) {
            i.operations.extend(m.operations.iter().cloned());
            i.attributes.extend(m.attributes.iter().cloned());
        }
    }
    Ok(model)
}

#[derive(Clone, Copy)]
enum Kind {
    Enum,
    Dictionary,
    Interface,
    Opaque,
}

struct Resolver<'m, 'a> {
    kinds: &'m BTreeMap<String, Kind>,
    typedefs: &'m BTreeMap<String, &'m Type<'a>>,
}

impl<'a> Resolver<'_, 'a> {
    fn interface_members(&self, i: &mut Interface, members: &[InterfaceMember<'a>]) {
        for m in members {
            match m {
                InterfaceMember::Operation(op) => {
                    // A static operation or a special one (a getter, a
                    // deleter) is none the wire calls on an object.
                    if op.modifier.is_some() || op.special.is_some() {
                        continue;
                    }
                    let Some(name) = op.identifier else {
                        continue;
                    };
                    i.operations.push(Operation {
                        name: name.0.to_owned(),
                        args: self.args(&op.args.body.list),
                        ret: self.ret(&op.return_type),
                    });
                }
                InterfaceMember::Attribute(a) => {
                    if a.modifier.is_some() {
                        continue;
                    }
                    i.attributes.push(Attribute {
                        name: a.identifier.0.to_owned(),
                        ty: self.ty(&a.type_.type_),
                        readonly: a.readonly.is_some(),
                    });
                }
                InterfaceMember::Setlike(s) => i.setlike = Some(self.ty(&s.generics.body.type_)),
                _ => {}
            }
        }
    }

    fn mixin_members(&self, i: &mut Interface, members: &[MixinMember<'a>]) {
        for m in members {
            match m {
                MixinMember::Operation(op) => {
                    let Some(name) = op.identifier else {
                        continue;
                    };
                    i.operations.push(Operation {
                        name: name.0.to_owned(),
                        args: self.args(&op.args.body.list),
                        ret: self.ret(&op.return_type),
                    });
                }
                MixinMember::Attribute(a) => i.attributes.push(Attribute {
                    name: a.identifier.0.to_owned(),
                    ty: self.ty(&a.type_.type_),
                    readonly: a.readonly.is_some(),
                }),
                _ => {}
            }
        }
    }

    fn args(&self, list: &[Argument<'a>]) -> Vec<Arg> {
        list.iter()
            .map(|a| match a {
                Argument::Single(s) => Arg {
                    name: s.identifier.0.to_owned(),
                    ty: self.ty(&s.type_.type_),
                    optional: s.optional.is_some(),
                    variadic: false,
                },
                Argument::Variadic(v) => Arg {
                    name: v.identifier.0.to_owned(),
                    ty: self.ty(&v.type_),
                    optional: true,
                    variadic: true,
                },
            })
            .collect()
    }

    fn ret(&self, ret: &ReturnType<'a>) -> Ty {
        match ret {
            ReturnType::Undefined(_) => Ty::Undefined,
            ReturnType::Type(t) => self.ty(t),
        }
    }

    fn ty(&self, ty: &Type<'a>) -> Ty {
        match ty {
            Type::Single(SingleType::Any(_)) => Ty::Opaque("any".into()),
            Type::Single(SingleType::NonAny(t)) => self.non_any(t),
            Type::Union(u) => {
                let alternatives = u
                    .type_
                    .body
                    .list
                    .iter()
                    .map(|m| match m {
                        UnionMemberType::Single(s) => self.non_any(&s.type_),
                        UnionMemberType::Union(inner) => self.ty(&Type::Union(inner.clone())),
                    })
                    .collect();
                nullable(Ty::Union(None, alternatives), u.q_mark.is_some())
            }
        }
    }

    fn non_any(&self, ty: &NonAnyType<'a>) -> Ty {
        use NonAnyType as N;
        match ty {
            N::Promise(p) => Ty::Promise(Box::new(self.ret(&p.generics.body))),
            N::Integer(i) => nullable(Ty::Integer(int(i.type_)), i.q_mark.is_some()),
            N::FloatingPoint(f) => {
                let double = matches!(f.type_, FloatingPointType::Double(_));
                nullable(Ty::Float(double), f.q_mark.is_some())
            }
            N::Boolean(b) => nullable(Ty::Boolean, b.q_mark.is_some()),
            N::Byte(b) => nullable(Ty::Integer(Int::I8), b.q_mark.is_some()),
            N::Octet(b) => nullable(Ty::Integer(Int::U8), b.q_mark.is_some()),
            N::ByteString(s) => nullable(Ty::String, s.q_mark.is_some()),
            N::DOMString(s) => nullable(Ty::String, s.q_mark.is_some()),
            N::USVString(s) => nullable(Ty::String, s.q_mark.is_some()),
            N::Sequence(s) => nullable(Ty::Sequence(Box::new(self.ty(&s.type_.generics.body))), s.q_mark.is_some()),
            N::FrozenArrayType(s) => {
                nullable(Ty::Sequence(Box::new(self.ty(&s.type_.generics.body))), s.q_mark.is_some())
            }
            N::RecordType(r) => {
                let (key, _, value) = &r.type_.generics.body;
                match **key {
                    RecordKeyType::Byte(_) | RecordKeyType::DOM(_) | RecordKeyType::USV(_) => {
                        nullable(Ty::Record(Box::new(self.ty(value))), r.q_mark.is_some())
                    }
                    RecordKeyType::NonAny(_) => Ty::Opaque("record".into()),
                }
            }
            N::ArrayBuffer(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::DataView(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Int8Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Int16Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Int32Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Uint8Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Uint16Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Uint32Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Uint8ClampedArray(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Float32Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Float64Array(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::ArrayBufferView(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::BufferSource(b) => nullable(Ty::Bytes, b.q_mark.is_some()),
            N::Object(_) | N::Symbol(_) | N::Error(_) => Ty::Opaque("object".into()),
            N::Identifier(id) => nullable(self.named(id.type_.0), id.q_mark.is_some()),
        }
    }

    fn named(&self, name: &str) -> Ty {
        if let Some(ty) = self.typedefs.get(name) {
            return match self.ty(ty) {
                Ty::Union(None, alternatives) => Ty::Union(Some(name.to_owned()), alternatives),
                Ty::Nullable(inner) => match *inner {
                    Ty::Union(None, alternatives) => {
                        Ty::Nullable(Box::new(Ty::Union(Some(name.to_owned()), alternatives)))
                    }
                    other => Ty::Nullable(Box::new(other)),
                },
                other => other,
            };
        }
        match (name, self.kinds.get(name)) {
            ("AllowSharedBufferSource", _) => Ty::Bytes,
            ("undefined", _) => Ty::Undefined,
            (_, Some(Kind::Enum)) => Ty::Enum(name.to_owned()),
            (_, Some(Kind::Dictionary)) => Ty::Dictionary(name.to_owned()),
            (_, Some(Kind::Interface)) => Ty::Interface(name.to_owned()),
            _ => Ty::Opaque(name.to_owned()),
        }
    }
}

fn nullable(ty: Ty, q_mark: bool) -> Ty {
    if q_mark { Ty::Nullable(Box::new(ty)) } else { ty }
}

fn int(i: IntegerType) -> Int {
    match i {
        IntegerType::LongLong(l) if l.unsigned.is_some() => Int::U64,
        IntegerType::LongLong(_) => Int::I64,
        IntegerType::Long(l) if l.unsigned.is_some() => Int::U32,
        IntegerType::Long(_) => Int::I32,
        IntegerType::Short(s) if s.unsigned.is_some() => Int::U16,
        IntegerType::Short(_) => Int::I16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Model {
        parse(include_str!("../../../plugins/cb_gpu/spec/webgpu.idl")).unwrap()
    }

    #[test]
    fn the_webgpu_idl_parses_whole() {
        let model = gpu();
        assert!(model.enums["GPUPowerPreference"].contains(&"high-performance".to_owned()));
        let device = model.interface("GPUDevice").unwrap();
        let create = device.operations.iter().find(|o| o.name == "createBuffer").unwrap();
        assert_eq!(create.args[0].ty, Ty::Dictionary("GPUBufferDescriptor".into()));
        assert_eq!(create.ret, Ty::Interface("GPUBuffer".into()));
        // Typedefs resolve: GPUSize64 is an unsigned long long.
        let size = model
            .dictionary_members("GPUBufferDescriptor")
            .into_iter()
            .find(|m| m.name == "size")
            .unwrap();
        assert_eq!(size.ty, Ty::Integer(Int::U64));
        // A mixin's members join the interface that includes it, and an
        // inherited dictionary's members come first.
        let pass = model.interface("GPURenderPassEncoder").unwrap();
        assert!(pass.operations.iter().any(|o| o.name == "setBindGroup"));
        let members = model.dictionary_members("GPUBufferDescriptor");
        assert_eq!(members[0].name, "label");
    }
}
