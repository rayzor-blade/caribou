//! What a Wren method exposes to the bridge, by attribute.
//!
//! Wren declares no types, so a method says what it takes and gives in a
//! wren_lift attribute, which the MIR and the class at run time keep:
//!
//! ```wren
//! #export = "add(n: Num) -> Num"
//! add(n) { _score = _score + n }
//! ```
//!
//! The value is the member's exported signature: the name other languages
//! see, which may differ from Wren's; a parameter per Wren parameter,
//! each `name`, `name: Type` or `_: Type` (`_` keeps Wren's name); and
//! `-> Type` for the result. A getter is `name -> Type` and a setter
//! `name=(v: Type)`. Types are Wren's own names, `Num`, `Bool`, `String`,
//! `List`, `Fn`, or a class of the same module; anything else, and an
//! undeclared parameter or result, is `Dyn`. Parameters match by position,
//! so a running class, which has no parameter names, reads the attribute
//! the same way the source does. The attribute is optional: a member
//! without one is exported under its own name with what inference gives.

use caribou::registry::TypeRef;
use wren_lift::ast::{Attribute, AttributeBody, AttributeLiteral};
use wren_lift::intern::Interner;
use wren_lift::mir::{AttrEntry, AttrValue};

/// The attribute carrying the exported signature.
pub const EXPORT: &str = "export";

/// One parameter of an exported signature: its name, unless `_`, and its
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportParam {
    pub name: Option<String>,
    pub ty: Option<String>,
}

/// An exported signature, parsed.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    pub params: Vec<ExportParam>,
    /// Whether the signature has a parameter list at all: `count` against
    /// `count()`.
    pub has_params: bool,
    /// Whether it is a setter, `name=(v)`.
    pub is_setter: bool,
    pub ret: Option<String>,
}

impl Export {
    /// Parse `name(a: T, b) -> R`, `name -> R` or `name=(v: T)`.
    pub fn parse(text: &str) -> Result<Export, String> {
        let text = text.trim();
        let (head, ret) = match text.split_once("->") {
            Some((h, r)) => (h.trim(), Some(r.trim())),
            None => (text, None),
        };
        if ret == Some("") {
            return Err(format!("`{text}`: nothing after `->`"));
        }
        let (name, params, has_params, is_setter) = if let Some(i) = head.find("=(") {
            let inner = head[i + 2..]
                .strip_suffix(')')
                .ok_or_else(|| format!("`{text}`: unclosed parameter list"))?;
            (&head[..i], inner, true, true)
        } else if let Some(i) = head.find('(') {
            let inner = head[i + 1..]
                .strip_suffix(')')
                .ok_or_else(|| format!("`{text}`: unclosed parameter list"))?;
            (&head[..i], inner, true, false)
        } else {
            (head, "", false, false)
        };
        let name = name.trim();
        if !is_identifier(name) {
            return Err(format!("`{text}`: `{name}` is not a name"));
        }
        let mut out = Vec::new();
        if !params.trim().is_empty() {
            for p in params.split(',') {
                let (pname, ty) = match p.split_once(':') {
                    Some((n, t)) => (n.trim(), Some(t.trim())),
                    None => (p.trim(), None),
                };
                if pname != "_" && !is_identifier(pname) {
                    return Err(format!("`{text}`: `{pname}` is not a parameter name"));
                }
                if ty == Some("") {
                    return Err(format!("`{text}`: `{pname}` has no type after `:`"));
                }
                out.push(ExportParam {
                    name: (pname != "_").then(|| pname.to_owned()),
                    ty: ty.map(str::to_owned),
                });
            }
        }
        if is_setter && out.len() != 1 {
            return Err(format!("`{text}`: a setter takes one parameter"));
        }
        Ok(Export {
            name: name.to_owned(),
            params: out,
            has_params,
            is_setter,
            ret: ret.map(str::to_owned),
        })
    }

    /// The export among the entries the VM keeps for a method.
    pub fn from_entries(entries: &[AttrEntry]) -> Result<Option<Export>, String> {
        for e in entries {
            if e.group.is_none() && e.key == EXPORT {
                return match &e.value {
                    Some(AttrValue::Str(s)) => Export::parse(s).map(Some),
                    _ => Err(format!("`#{EXPORT}` takes a signature string")),
                };
            }
        }
        Ok(None)
    }

    /// The export among a method's attributes in source.
    pub fn from_ast(attrs: &[Attribute], interner: &Interner) -> Result<Option<Export>, String> {
        for a in attrs.iter().filter(|a| a.is_runtime) {
            if interner.resolve(a.name.0) != EXPORT {
                continue;
            }
            return match &a.body {
                AttributeBody::Value((AttributeLiteral::Str(s), _)) => Export::parse(s).map(Some),
                _ => Err(format!("`#{EXPORT}` takes a signature string")),
            };
        }
        Ok(None)
    }

    /// The type of the parameter at `index`, in `module`, whose classes
    /// are `classes`.
    pub fn param(&self, index: usize, module: &str, classes: &[String]) -> TypeRef {
        self.params
            .get(index)
            .and_then(|p| p.ty.as_deref())
            .map_or(TypeRef::Dyn, |t| type_ref(t, module, classes))
    }

    pub fn ret(&self, module: &str, classes: &[String]) -> Option<TypeRef> {
        self.ret.as_deref().map(|t| type_ref(t, module, classes))
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A Wren type name as a registry type.
pub fn type_ref(name: &str, module: &str, classes: &[String]) -> TypeRef {
    match name {
        "Num" => TypeRef::Float,
        "Bool" => TypeRef::Bool,
        "String" => TypeRef::Str,
        "List" => TypeRef::Array(Box::new(TypeRef::Dyn)),
        "Fn" => TypeRef::Fun,
        _ if classes.iter().any(|c| c == name) => TypeRef::Object(format!("{module}.{name}")),
        _ => TypeRef::Dyn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_parse_into_name_parameters_and_result() {
        let e = Export::parse("hit(n: Num, other: Hud) -> Bool").unwrap();
        assert_eq!(e.name, "hit");
        assert_eq!(e.params.len(), 2);
        assert_eq!(e.params[0].name.as_deref(), Some("n"));
        assert_eq!(e.params[1].ty.as_deref(), Some("Hud"));
        assert_eq!(e.ret.as_deref(), Some("Bool"));
        assert!(e.has_params && !e.is_setter);
        let classes = vec!["Hud".to_owned()];
        assert_eq!(e.param(0, "hud", &classes), TypeRef::Float);
        assert_eq!(
            e.param(1, "hud", &classes),
            TypeRef::Object("hud.Hud".to_owned())
        );
        assert_eq!(e.param(2, "hud", &classes), TypeRef::Dyn);
        assert_eq!(e.ret("hud", &classes), Some(TypeRef::Bool));

        let e = Export::parse("score -> Num").unwrap();
        assert!(!e.has_params && e.params.is_empty());
        let e = Export::parse("score=(v: Num)").unwrap();
        assert!(e.is_setter && e.params[0].ty.as_deref() == Some("Num"));
        let e = Export::parse("f(_: Num, b)").unwrap();
        assert_eq!(e.params[0].name, None);
        assert_eq!(
            e.params[1],
            ExportParam {
                name: Some("b".into()),
                ty: None
            }
        );
        assert_eq!(Export::parse("count()").unwrap().params.len(), 0);

        for bad in ["", "1abc()", "f(a:)", "f(a", "s=(a, b)", "f() ->"] {
            assert!(Export::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_attribute_is_read_from_source_and_from_the_runtime_entries() {
        let source = "class Hud {\n  #export = \"add(n: Num) -> Num\"\n  hit(n) { n }\n}\n";
        let parsed = wren_lift::parse::parser::parse(source);
        let wren_lift::ast::Stmt::Class(class) = &parsed.module[0].0 else {
            panic!("a class");
        };
        let e = Export::from_ast(&class.methods[0].0.attributes, &parsed.interner)
            .unwrap()
            .expect("exported");
        assert_eq!(e.name, "add");
        let entries = vec![AttrEntry {
            group: None,
            key: EXPORT.to_owned(),
            value: Some(AttrValue::Str("add(n: Num) -> Num".to_owned())),
        }];
        assert_eq!(Export::from_entries(&entries).unwrap(), Some(e));
        assert_eq!(Export::from_entries(&[]).unwrap(), None);
        let flag = vec![AttrEntry {
            group: None,
            key: EXPORT.to_owned(),
            value: None,
        }];
        assert!(Export::from_entries(&flag).is_err());
    }
}
