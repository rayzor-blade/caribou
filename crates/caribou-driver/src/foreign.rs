//! Haxe classes that compiled Wren imports (docs/architecture/linking.md).
//!
//! An import of a Haxe module becomes the module `haxe:<module>`: a class
//! WrenLift makes whose members call link symbols, and those symbols Ash
//! exports as thunks over the Haxe members, casting each value between
//! the two languages' forms on Ash's side, where the program's types are.

use ash_core::host_export::{ExportKind, HostExport};
use caribou::describe::{MemberKind, ModuleDesc};
use caribou::link::{Kind, symbol};
use caribou::registry::TypeRef;
use wren_lift::codegen::aot::AotModule;
use wren_lift::codegen::llvm_aot::{AotForeignClass, AotForeignMember, AotForeignModule};
use wren_lift::runtime::value::Value;

/// What a Haxe exception thrown in an exported member becomes: the error
/// of the Wren call.
pub const RAISE: &str = "caribou_wren_raise_haxe";

/// The Haxe object casts, receiver included.
const OBJECT: (&str, &str) = (
    "caribou_wren_from_haxe_object",
    "caribou_wren_to_haxe_object",
);

/// Point each import of a Haxe module in `modules` at `haxe:<module>`, and
/// say what those modules are and which Haxe members they call. An import
/// `ns:module` names the Haxe module `module`, or `ns.module`, as a hosted
/// run's namespace does.
pub fn plan(
    modules: &mut [AotModule],
    haxe: &[ModuleDesc],
) -> (Vec<AotForeignModule>, Vec<HostExport>) {
    let mut used: Vec<&ModuleDesc> = Vec::new();
    for module in modules.iter_mut() {
        for source in module.module_var_sources.iter_mut().flatten() {
            let Some((ns, name)) = source.module.split_once(':') else {
                continue;
            };
            if ns.is_empty() || ns.starts_with('@') {
                continue;
            }
            let qualified = format!("{ns}.{name}");
            let Some(desc) = haxe
                .iter()
                .find(|d| d.module == name || d.module == qualified)
            else {
                continue;
            };
            source.module = format!("haxe:{}", desc.module);
            if !used.iter().any(|d| d.module == desc.module) {
                used.push(desc);
            }
        }
    }
    let mut foreign = Vec::new();
    let mut exports = Vec::new();
    for desc in used {
        for class in &desc.classes {
            let mut members = Vec::new();
            let mut add = |signature: String,
                           is_static: bool,
                           kind: Kind,
                           member: &str,
                           params: Vec<&TypeRef>,
                           ret: &TypeRef| {
                let receiver = !is_static;
                let mut arg_casts = Vec::new();
                if receiver {
                    arg_casts.push(OBJECT.1.to_owned());
                }
                for param in &params {
                    let Some((_, to_haxe)) = casts(param, haxe) else {
                        return;
                    };
                    arg_casts.push(to_haxe.to_owned());
                }
                let ret_cast = match ret {
                    TypeRef::Void => None,
                    ret => match casts(ret, haxe) {
                        Some((from_haxe, _)) => Some(from_haxe.to_owned()),
                        None => return,
                    },
                };
                let arity = params.len();
                let symbol = symbol("haxe", &desc.module, &class.name, kind, member, arity);
                members.push(AotForeignMember {
                    signature,
                    is_static,
                    symbol: symbol.clone(),
                });
                exports.push(HostExport {
                    symbol,
                    class: class.type_name.clone(),
                    member: member.to_owned(),
                    kind: match kind {
                        Kind::Static => ExportKind::Static,
                        Kind::Method => ExportKind::Method,
                        Kind::Constructor => ExportKind::Constructor,
                        Kind::Getter => ExportKind::Getter,
                        Kind::Setter => ExportKind::Setter,
                    },
                    arg_casts: arg_casts.into_iter().map(Some).collect(),
                    ret_cast,
                    raise: RAISE.to_owned(),
                    unit: Value::null().to_bits(),
                });
            };
            for m in &class.members {
                let params: Vec<&TypeRef> = m.params.iter().map(|p| &p.ty).collect();
                let blanks = vec!["_"; params.len()].join(",");
                let (is_static, kind) = match m.kind {
                    MemberKind::Constructor => (true, Kind::Constructor),
                    MemberKind::Static | MemberKind::Factory => (true, Kind::Static),
                    MemberKind::Method => (false, Kind::Method),
                    MemberKind::Getter | MemberKind::Setter => continue,
                };
                add(
                    format!("{}({blanks})", m.name),
                    is_static,
                    kind,
                    &m.name,
                    params,
                    &m.ret,
                );
            }
            for f in &class.fields {
                add(
                    f.name.clone(),
                    false,
                    Kind::Getter,
                    &f.name,
                    Vec::new(),
                    &f.ty,
                );
                add(
                    format!("{}=(_)", f.name),
                    false,
                    Kind::Setter,
                    &f.name,
                    vec![&f.ty],
                    &TypeRef::Void,
                );
            }
            foreign.push(AotForeignModule {
                name: format!("haxe:{}", desc.module),
                classes: vec![AotForeignClass {
                    name: class.name.clone(),
                    members,
                }],
            });
        }
    }
    (foreign, exports)
}

/// The casts for a value of `ty` crossing between Haxe and Wren: from
/// Haxe's form to Wren's, and back. `None` for a type with no static form,
/// which stays on the bridge.
fn casts(ty: &TypeRef, haxe: &[ModuleDesc]) -> Option<(&'static str, &'static str)> {
    Some(match ty {
        // Ash converts a NaN-boxed number inline.
        TypeRef::Float => ("ash:box_f64", "ash:unbox_f64"),
        TypeRef::Int => ("caribou_wren_from_int", "caribou_wren_to_int"),
        TypeRef::Bool => ("caribou_wren_from_bool", "caribou_wren_to_bool"),
        TypeRef::Str => (
            "caribou_wren_from_haxe_string",
            "caribou_wren_to_haxe_string",
        ),
        TypeRef::Buffer => ("caribou_wren_from_haxe_bytes", "caribou_wren_to_haxe_bytes"),
        TypeRef::Fun | TypeRef::Function { .. } => (
            "caribou_wren_from_haxe_function",
            "caribou_wren_to_haxe_function",
        ),
        TypeRef::Object(name)
            if haxe
                .iter()
                .flat_map(|d| &d.classes)
                .any(|c| &c.type_name == name) =>
        {
            OBJECT
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use caribou::describe::{ClassDesc, FieldDesc, MemberDesc, ParamDesc};

    fn member(name: &str, kind: MemberKind, params: &[TypeRef], ret: TypeRef) -> MemberDesc {
        MemberDesc {
            name: name.to_owned(),
            kind,
            signature: String::new(),
            params: params
                .iter()
                .map(|ty| ParamDesc {
                    name: "a".to_owned(),
                    ty: ty.clone(),
                })
                .collect(),
            ret,
            exported: false,
        }
    }

    #[test]
    fn a_haxe_import_becomes_a_foreign_class_and_its_exports() {
        let bench = ModuleDesc {
            lang: "haxe".to_owned(),
            module: "Bench".to_owned(),
            classes: vec![ClassDesc {
                name: "Bench".to_owned(),
                type_name: "Bench".to_owned(),
                superclass: None,
                fields: vec![FieldDesc {
                    name: "v".to_owned(),
                    ty: TypeRef::Float,
                }],
                statics: Vec::new(),
                members: vec![
                    member(
                        "new",
                        MemberKind::Constructor,
                        &[],
                        TypeRef::Object("Bench".into()),
                    ),
                    member("add", MemberKind::Static, &[TypeRef::Float], TypeRef::Float),
                    member("bump", MemberKind::Method, &[TypeRef::Float], TypeRef::Void),
                    member("any", MemberKind::Static, &[TypeRef::Dyn], TypeRef::Void),
                ],
            }],
            enums: Vec::new(),
            functions: Vec::new(),
            path: None,
        };
        let tally =
            "import \"bench:Bench\" for Bench\nclass Tally {\n  static go() { Bench.add(1) }\n}\n";
        let dir = std::env::temp_dir().join(format!("caribou-foreign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tally.wren");
        std::fs::write(&path, tally).unwrap();
        let mut modules = wren_lift::codegen::aot::walk_imports(&path)
            .unwrap()
            .modules;
        let (foreign, exports) = plan(&mut modules, &[bench]);
        std::fs::remove_dir_all(&dir).ok();

        let source = modules[0]
            .module_var_sources
            .iter()
            .flatten()
            .next()
            .unwrap();
        assert_eq!(source.module, "haxe:Bench");
        assert_eq!(foreign.len(), 1);
        let signatures: Vec<(&str, bool)> = foreign[0].classes[0]
            .members
            .iter()
            .map(|m| (m.signature.as_str(), m.is_static))
            .collect();
        // `any` takes a Dyn, which has no static form.
        assert_eq!(
            signatures,
            [
                ("new()", true),
                ("add(_)", true),
                ("bump(_)", false),
                ("v", false),
                ("v=(_)", false)
            ]
        );
        let add = exports.iter().find(|e| e.member == "add").unwrap();
        assert_eq!(add.symbol, "caribou_4haxe_5Bench_5Bench_t3add_1");
        assert_eq!(add.arg_casts, [Some("ash:unbox_f64".to_owned())]);
        assert_eq!(add.ret_cast.as_deref(), Some("ash:box_f64"));
        let bump = exports.iter().find(|e| e.member == "bump").unwrap();
        assert_eq!(
            bump.arg_casts,
            [Some(OBJECT.1.to_owned()), Some("ash:unbox_f64".to_owned())]
        );
        assert_eq!(bump.ret_cast, None);
        let new = exports.iter().find(|e| e.member == "new").unwrap();
        assert_eq!(new.ret_cast.as_deref(), Some(OBJECT.0));
    }
}
