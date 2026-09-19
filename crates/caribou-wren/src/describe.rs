//! A Wren module's interface from its source, before anything runs: what
//! `caribou-wren --describe` prints for a build step to declare the
//! module's classes in another language.
//!
//! The classes and members are read from the parse tree with the rules
//! `publish.rs` applies to the running VM: every class the module defines,
//! its superclass unless that is Object, each member with its Wren
//! signature, operators and subscripts left out, the constructor `new` as
//! the constructor and any other as a factory. A member's `#export`
//! attribute (`types.rs`) gives its exported name, parameter names and
//! types, and result type; what it leaves out is Wren's own name, the
//! source's parameter names, and the result wren_lift's inference gives
//! (a literal, an interpolation, a constructor call, a field or another
//! method), else `Dyn`. A parameter's type is only ever declared.

use std::collections::HashSet;

use caribou::describe::{ClassDesc, MemberDesc, MemberKind, ModuleDesc, ParamDesc};
use caribou::registry::TypeRef;
use wren_lift::ast::{ClassDecl, MethodSig, Stmt};
use wren_lift::diagnostics::Severity;
use wren_lift::intern::{Interner, SymbolId};
use wren_lift::sema::types::{InferredType, TypeEnv, infer_types_with_classes};

use crate::types::{Classes, Export};

/// The language name the description carries.
pub const LANG: &str = "wren";

/// Describe `source` as module `module`. A parse error, or an `#export`
/// that does not fit its member, is the message.
pub fn describe_source(module: &str, source: &str) -> Result<ModuleDesc, String> {
    let parsed = wren_lift::parse::parser::parse(source);
    if let Some(e) = parsed.errors.iter().find(|d| d.severity == Severity::Error) {
        return Err(e.message.clone());
    }
    let interner = &parsed.interner;
    let classes: Vec<&ClassDecl> = parsed
        .module
        .iter()
        .filter_map(|s| match &s.0 {
            Stmt::Class(c) => Some(c),
            _ => None,
        })
        .collect();
    let names: Vec<String> = classes
        .iter()
        .map(|c| interner.resolve(c.name.0).to_owned())
        .collect();
    // A class a namespaced import brings in, by the name it is imported
    // as: its registry name is not known from here, so it is written as
    // the import and the class, `swarm:Entity.Entity`, for the build
    // macro to resolve; the publisher resolves it on the running VM.
    let mut imported: Vec<(String, String)> = Vec::new();
    for (stmt, _) in &parsed.module {
        let Stmt::Import { module, names } = stmt else {
            continue;
        };
        let (path, _) = module;
        if !path.contains(':') {
            continue;
        }
        for n in names {
            let name = interner.resolve(n.name.0);
            let local = n
                .alias
                .as_ref()
                .map_or(name, |(alias, _)| interner.resolve(*alias));
            imported.push((local.to_owned(), format!("{path}.{name}")));
        }
    }
    let classes_here = Classes {
        module,
        own: &names,
        imported: &imported,
    };
    // What wren_lift can tell about results, seeded as its hover is.
    let known: HashSet<SymbolId> = classes.iter().map(|c| c.name.0).collect();
    let env = infer_types_with_classes(&parsed.module, known, interner.lookup("new"));
    Ok(ModuleDesc {
        lang: LANG.to_owned(),
        module: module.to_owned(),
        path: None,
        functions: Vec::new(),
        classes: classes
            .iter()
            .map(|c| describe_class(c, module, &classes_here, interner, &env))
            .collect::<Result<_, _>>()?,
    })
}

/// An inferred type as a registry type; `Any` is `Dyn`.
fn inferred(ty: &InferredType, classes: &Classes<'_>, interner: &Interner) -> TypeRef {
    match ty {
        InferredType::Num => TypeRef::Float,
        InferredType::Bool => TypeRef::Bool,
        InferredType::String => TypeRef::Str,
        InferredType::List => TypeRef::Array(Box::new(TypeRef::Dyn)),
        InferredType::Fn => TypeRef::Fun,
        InferredType::Class(sym) => crate::types::type_ref(interner.resolve(*sym), classes),
        InferredType::Null | InferredType::Map | InferredType::Range | InferredType::Any => {
            TypeRef::Dyn
        }
    }
}

fn describe_class(
    c: &ClassDecl,
    module: &str,
    classes: &Classes<'_>,
    interner: &Interner,
    env: &TypeEnv,
) -> Result<ClassDesc, String> {
    let name = interner.resolve(c.name.0).to_owned();
    let type_name = format!("{module}.{name}");
    let superclass = c
        .superclass
        .as_ref()
        .map(|s| interner.resolve(s.0).to_owned())
        .filter(|s| s != "Object");
    let mut members = Vec::new();
    let mut has_new = false;
    for (m, _) in &c.methods {
        let (base, params, shape) = match &m.signature {
            MethodSig::Named { name, params } => (*name, params.as_slice(), Kind::Method),
            MethodSig::Getter(name) => (*name, &[][..], Kind::Getter),
            MethodSig::Setter { name, param } => (*name, std::slice::from_ref(param), Kind::Setter),
            MethodSig::Construct { name, params } => (*name, params.as_slice(), Kind::Construct),
            MethodSig::Subscript { .. }
            | MethodSig::SubscriptSetter { .. }
            | MethodSig::Operator { .. } => continue,
        };
        let base_sym = base;
        let base = interner.resolve(base).to_owned();
        let export = Export::from_ast(&m.attributes, interner)
            .and_then(|e| e.map_or(Ok(None), |e| check(e, shape, params.len()).map(Some)))
            .map_err(|e| format!("{name}.{base}: {e}"))?;
        let param_names: Vec<&str> = params.iter().map(|p| interner.resolve(p.0)).collect();
        let kind = match shape {
            Kind::Construct if base == "new" && !has_new => {
                has_new = true;
                MemberKind::Constructor
            }
            Kind::Construct => MemberKind::Factory,
            Kind::Method if m.is_static => MemberKind::Static,
            Kind::Method => MemberKind::Method,
            Kind::Getter if m.is_static => MemberKind::Static,
            Kind::Getter => MemberKind::Getter,
            Kind::Setter if m.is_static => MemberKind::Static,
            Kind::Setter => MemberKind::Setter,
        };
        let signature = match shape {
            Kind::Getter => base.clone(),
            Kind::Setter => format!("{base}=(_)"),
            Kind::Method | Kind::Construct => {
                let blanks: Vec<&str> = std::iter::repeat_n("_", params.len()).collect();
                format!("{base}({})", blanks.join(","))
            }
        };
        let exported_ret = export.as_ref().and_then(|e| e.ret(classes));
        let ret = match kind {
            MemberKind::Constructor | MemberKind::Factory => TypeRef::Object(type_name.clone()),
            _ if exported_ret.is_some() => exported_ret.unwrap_or(TypeRef::Dyn),
            // A one-expression body is its expression, which the inferrer
            // types but does not record as the method's result.
            _ => {
                let ty = match &m.body {
                    Some((Stmt::Expr(e), _)) => env.get_expr_type(e.1.start),
                    _ => env.get_method_return_type(c.name.0, base_sym),
                };
                inferred(ty, classes, interner)
            }
        };
        let exported_name = |i: usize| {
            export
                .as_ref()
                .and_then(|e| e.params.get(i))
                .and_then(|p| p.name.clone())
        };
        members.push(MemberDesc {
            name: export.as_ref().map_or(base, |e| e.name.clone()),
            kind,
            signature,
            params: param_names
                .iter()
                .enumerate()
                .map(|(i, pname)| ParamDesc {
                    name: exported_name(i).unwrap_or_else(|| (*pname).to_owned()),
                    ty: export
                        .as_ref()
                        .map_or(TypeRef::Dyn, |e| e.param(i, classes)),
                })
                .collect(),
            ret,
        });
    }
    Ok(ClassDesc {
        name,
        type_name,
        superclass,
        fields: Vec::new(),
        statics: Vec::new(),
        members,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Method,
    Getter,
    Setter,
    Construct,
}

/// An export fits its member when its shape and arity are the member's.
fn check(e: Export, shape: Kind, arity: usize) -> Result<Export, String> {
    let fits = match shape {
        Kind::Getter => !e.has_params,
        Kind::Setter => e.is_setter,
        Kind::Method | Kind::Construct => e.has_params && !e.is_setter,
    };
    if !fits {
        return Err(format!(
            "`#export = \"{}\"` has the wrong shape for the member",
            e.name
        ));
    }
    if e.params.len() != arity {
        return Err(format!(
            "`#export` names {} parameters, the member takes {arity}",
            e.params.len()
        ));
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUD: &str = r#"
class Hud {
  construct new(score) { _score = score }
  construct blank() { _score = 0 }
  #export = "add(n: Num) -> Num"
  add(n) { _score = _score + n }
  score { _score }
  #export = "score=(v: Num)"
  score=(v) { _score = v }
  label(prefix) { "%(prefix): %(_score)" }
  #export = "explode()"
  fail() { Fiber.abort("boom") }
  #export = "best(_: Hud, second: Hud) -> Hud"
  static best(a, b) { a }
  static count { 0 }
  static blank() { Hud.new(0) }
  static flag { true }
  +(other) { this }
  [i] { i }
}
class Panel is Hud {
  construct new() { super(0) }
}
"#;

    #[test]
    fn a_module_describes_its_classes_from_source() {
        let d = describe_source("hud", HUD).unwrap();
        assert_eq!((d.lang.as_str(), d.module.as_str()), ("wren", "hud"));
        let hud = &d.classes[0];
        assert_eq!(hud.type_name, "hud.Hud");
        assert_eq!(hud.superclass, None);
        let by_name = |n: &str, k: MemberKind| {
            hud.members
                .iter()
                .find(|m| m.name == n && m.kind == k)
                .unwrap_or_else(|| panic!("{n}"))
        };
        let new = by_name("new", MemberKind::Constructor);
        assert_eq!(new.signature, "new(_)");
        assert_eq!(new.params[0].name, "score");
        assert_eq!(new.ret, TypeRef::Object("hud.Hud".to_owned()));
        let blank = by_name("blank", MemberKind::Factory);
        assert_eq!(blank.signature, "blank()");
        let add = by_name("add", MemberKind::Method);
        assert_eq!(add.signature, "add(_)");
        assert_eq!(add.params[0].ty, TypeRef::Float);
        assert_eq!(add.ret, TypeRef::Float);
        assert_eq!(by_name("score", MemberKind::Getter).signature, "score");
        let setter = by_name("score", MemberKind::Setter);
        assert_eq!(setter.signature, "score=(_)");
        assert_eq!(setter.params[0].ty, TypeRef::Float);
        // An export renames the member; the bridge still asks for Wren's.
        let explode = by_name("explode", MemberKind::Method);
        assert_eq!(explode.signature, "fail()");
        let best = by_name("best", MemberKind::Static);
        assert_eq!(best.signature, "best(_,_)");
        assert_eq!(best.ret, TypeRef::Object("hud.Hud".to_owned()));
        // `_` keeps the source's parameter name; a name replaces it.
        assert_eq!(best.params[0].name, "a");
        assert_eq!(best.params[1].name, "second");
        assert_eq!(best.params[1].ty, TypeRef::Object("hud.Hud".to_owned()));
        assert_eq!(by_name("count", MemberKind::Static).signature, "count");
        assert!(hud.members.iter().all(|m| m.name != "+" && m.name != "[_]"));
        // Results wren_lift infers need no attribute.
        assert_eq!(by_name("label", MemberKind::Method).ret, TypeRef::Str);
        assert_eq!(by_name("count", MemberKind::Static).ret, TypeRef::Float);
        assert_eq!(by_name("flag", MemberKind::Static).ret, TypeRef::Bool);
        assert_eq!(
            by_name("blank", MemberKind::Static).ret,
            TypeRef::Object("hud.Hud".to_owned())
        );
        // A field's type is not known: its constructor's parameter is not.
        assert_eq!(by_name("score", MemberKind::Getter).ret, TypeRef::Dyn);
        assert_eq!(d.classes[1].superclass.as_deref(), Some("Hud"));
    }

    #[test]
    fn a_parse_error_is_the_answer() {
        assert!(describe_source("bad", "class {").is_err());
    }

    #[test]
    fn an_export_may_name_a_class_a_namespaced_import_brings_in() {
        let source = "import \"swarm:Entity\" for Entity\nimport \"swarm:World\" for World as W\nimport \"lib\" for Plain\n\
            class Boid {\n  #export = \"new(e: Entity, w: W, p: Plain)\"\n  construct new(e, w, p) {}\n}\n";
        let d = describe_source("game", source).unwrap();
        let ctor = &d.classes[0].members[0];
        assert_eq!(
            ctor.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
            vec![
                TypeRef::Object("swarm:Entity.Entity".to_owned()),
                TypeRef::Object("swarm:World.World".to_owned()),
                // A plain import's home is not known from here.
                TypeRef::Dyn,
            ]
        );
    }

    #[test]
    fn an_export_must_fit_its_member() {
        let source = "class A {\n  #export = \"f(a: Num)\"\n  f(a, b) { a }\n}\n";
        let err = describe_source("m", source).unwrap_err();
        assert!(err.contains("takes 2"), "{err}");
        let source = "class A {\n  #export = \"g()\"\n  g { 1 }\n}\n";
        let err = describe_source("m", source).unwrap_err();
        assert!(err.contains("shape"), "{err}");
    }
}
