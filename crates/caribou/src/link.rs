//! What a published member is at link time: the symbol an AOT build of
//! either language defines or references for it, and the C signature the
//! member's types dictate. Both runtimes' AOTs derive from this, so the
//! two sides agree by construction; see `docs/architecture/linking.md`.

use crate::registry::{ClassIface, Interface, MethodIface, TypeRef};
use crate::world::language_name;

/// A C type a value crosses as at link time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CType {
    /// No result.
    Void,
    /// `double`.
    Double,
    /// `int32_t`.
    Int32,
    /// `bool`, one byte.
    Bool,
    /// `caribou_str *`: a core string.
    Str,
    /// `void *`: an object as its own language holds it.
    Object,
    /// `uint64_t`: a bridge value, for what has no C form of its own.
    Value,
}

impl CType {
    /// The type as C spells it.
    pub fn spelling(self) -> &'static str {
        match self {
            CType::Void => "void",
            CType::Double => "double",
            CType::Int32 => "int32_t",
            CType::Bool => "bool",
            CType::Str => "caribou_str *",
            CType::Object => "void *",
            CType::Value => "uint64_t",
        }
    }

    /// The C type for a registry type; `None` for a type with no static
    /// form, which keeps its member on the bridge.
    pub fn of(ty: &TypeRef) -> Option<CType> {
        Some(match ty {
            TypeRef::Void => CType::Void,
            TypeRef::Bool => CType::Bool,
            TypeRef::Int => CType::Int32,
            TypeRef::Float => CType::Double,
            TypeRef::Str => CType::Str,
            TypeRef::Object(_) => CType::Object,
            // A list and a typed function cross as the bridge values they
            // are; only their use inside the callee is dynamic.
            TypeRef::Array(_) | TypeRef::Function { .. } => CType::Value,
            TypeRef::Dyn | TypeRef::Fun => return None,
        })
    }
}

/// What a member is to its class in the symbol: the one letter after the
/// class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Method,
    Getter,
    Setter,
    Static,
    Constructor,
}

impl Kind {
    fn letter(self) -> char {
        match self {
            Kind::Method => 'm',
            Kind::Getter => 'g',
            Kind::Setter => 's',
            Kind::Static => 't',
            Kind::Constructor => 'c',
        }
    }
}

/// A member at link time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The C symbol.
    pub symbol: String,
    /// The parameters' C types, the receiver first for an instance member.
    pub params: Vec<CType>,
    pub ret: CType,
    /// The parameters that have no static type, counted from one, and 0
    /// for the result. Empty when the member links; otherwise its thunk
    /// calls the bridge, and a report can say which type to declare.
    pub dynamic: Vec<usize>,
}

impl Link {
    /// Whether the member links statically: every type has a C form.
    pub fn is_static(&self) -> bool {
        self.dynamic.is_empty()
    }

    /// The C declaration.
    pub fn declaration(&self) -> String {
        let params = if self.params.is_empty() {
            "void".to_owned()
        } else {
            self.params
                .iter()
                .map(|p| p.spelling())
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!("{} {}({params});", self.ret.spelling(), self.symbol)
    }
}

/// The symbol for a member: `caribou`, then the language, the module as
/// its language spells it, the class, the kind's letter with the member's
/// name, and the arity, each after a `_`, and each name as its length
/// and its text. A character that is not a C identifier's is written as
/// `_` and two hex digits, `_` itself included, so no two names share a
/// symbol and the separators stay readable.
pub fn symbol(
    lang: &str,
    module: &str,
    class: &str,
    kind: Kind,
    name: &str,
    arity: usize,
) -> String {
    let mut out = String::from("caribou");
    for part in [lang, module, class] {
        out.push('_');
        segment(&mut out, part);
    }
    out.push('_');
    out.push(kind.letter());
    segment(&mut out, name);
    out.push('_');
    out.push_str(&arity.to_string());
    out
}

fn segment(out: &mut String, text: &str) {
    let escaped = escape(text);
    out.push_str(&escaped.len().to_string());
    out.push_str(&escaped);
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() {
            out.push(b as char);
        } else {
            out.push_str(&format!("_{b:02x}"));
        }
    }
    out
}

/// The link for `member` of `class` in `iface`.
pub fn link(iface: &Interface, class: &ClassIface, member: &MethodIface, kind: Kind) -> Link {
    let lang = language_name(iface.lang);
    let mut params = Vec::with_capacity(member.params.len() + 1);
    let mut dynamic = Vec::new();
    if matches!(kind, Kind::Method | Kind::Getter | Kind::Setter) {
        params.push(CType::Object);
    }
    for (i, p) in member.params.iter().enumerate() {
        match CType::of(p) {
            Some(t) => params.push(t),
            None => {
                params.push(CType::Value);
                dynamic.push(i + 1);
            }
        }
    }
    let ret = match kind {
        Kind::Constructor => CType::Object,
        Kind::Setter => CType::Void,
        _ => match CType::of(&member.ret) {
            Some(t) => t,
            None => {
                dynamic.push(0);
                CType::Value
            }
        },
    };
    Link {
        symbol: symbol(
            &lang,
            &iface.module,
            &class.name,
            kind,
            &member.name,
            member.params.len(),
        ),
        params,
        ret,
        dynamic,
    }
}

/// Every member of `iface` with its link: what an AOT build defines for
/// the module, or references when it imports from it.
pub fn plan(iface: &Interface) -> Vec<(String, Link)> {
    let mut out = Vec::new();
    for class in &iface.classes {
        if let Some(ctor) = &class.ctor {
            let l = link(iface, class, ctor, Kind::Constructor);
            out.push((format!("{}.new", class.name), l));
        }
        for m in &class.methods {
            let kind = if m.is_static {
                Kind::Static
            } else {
                match m.kind() {
                    crate::registry::MethodKind::Method => Kind::Method,
                    crate::registry::MethodKind::Getter => Kind::Getter,
                    crate::registry::MethodKind::Setter => Kind::Setter,
                }
            };
            let l = link(iface, class, m, kind);
            out.push((format!("{}.{}", class.name, m.name), l));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Callable;
    use caribou_abi::Value;

    fn method(name: &str, is_static: bool, params: Vec<TypeRef>, ret: TypeRef) -> MethodIface {
        MethodIface {
            name: name.to_owned(),
            is_static,
            params,
            ret,
            target: Callable::Dynamic(Value::null()),
        }
    }

    #[test]
    fn a_symbol_spells_every_part_with_its_length() {
        assert_eq!(
            symbol("wren", "bench/tally", "Tally", Kind::Method, "add", 1),
            "caribou_4wren_13bench_2ftally_5Tally_m3add_1"
        );
        assert_eq!(
            symbol("haxe", "game.Player", "Player", Kind::Static, "spawnAt", 2),
            "caribou_4haxe_13game_2ePlayer_6Player_t7spawnAt_2"
        );
        // A setter and a getter of one name differ by the letter; an
        // operator's characters are escaped, `_` included.
        assert_eq!(
            symbol("wren", "m", "C", Kind::Setter, "hp", 1),
            "caribou_4wren_1m_1C_s2hp_1"
        );
        assert_eq!(
            symbol("wren", "m", "C", Kind::Method, "+", 1),
            "caribou_4wren_1m_1C_m3_2b_1"
        );
        assert_eq!(
            symbol("wren", "m", "C", Kind::Method, "a_b", 0),
            "caribou_4wren_1m_1C_m5a_5fb_0"
        );
    }

    #[test]
    fn a_link_has_the_c_types_and_names_what_stays_dynamic() {
        let class = ClassIface {
            name: "Tally".to_owned(),
            type_name: "bench/tally.Tally".to_owned(),
            superclass: None,
            fields: Vec::new(),
            statics: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            class_object: Value::null(),
        };
        let iface = Interface {
            lang: crate::world::LANG_CORE,
            module: "bench/tally".to_owned(),
            classes: vec![class.clone()],
            functions: Vec::new(),
        };
        let add = method("add", false, vec![TypeRef::Float], TypeRef::Float);
        let l = link(&iface, &class, &add, Kind::Method);
        assert_eq!(l.params, vec![CType::Object, CType::Double]);
        assert_eq!(l.ret, CType::Double);
        assert!(l.is_static());
        assert_eq!(
            l.declaration(),
            format!("double {}(void *, double);", l.symbol)
        );

        let untyped = method(
            "apply",
            true,
            vec![TypeRef::Fun, TypeRef::Int],
            TypeRef::Dyn,
        );
        let l = link(&iface, &class, &untyped, Kind::Static);
        assert_eq!(l.params, vec![CType::Value, CType::Int32]);
        assert_eq!(l.ret, CType::Value);
        assert_eq!(l.dynamic, vec![1, 0]);

        let make = method("new", true, vec![TypeRef::Str], TypeRef::Object("x".into()));
        let l = link(&iface, &class, &make, Kind::Constructor);
        assert_eq!(l.params, vec![CType::Str]);
        assert_eq!(l.ret, CType::Object);
        assert_eq!(
            l.declaration(),
            format!("void * {}(caribou_str *);", l.symbol)
        );
    }
}
