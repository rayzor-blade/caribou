//! What a Wren method exposes to the bridge, by attribute, as registry
//! types.
//!
//! ```wren
//! #export = "add(n: Num) -> Num"
//! add(n) { _score = _score + n }
//! ```
//!
//! The attribute and its grammar are WrenLift's
//! (`wren_lift::sema::export`), which also checks it against its member
//! and enforces the types it declares. This maps its type names to the
//! registry's: `Num`, `Bool`, `String`, `Null`, `List`, `Fn`, a function
//! of a shape, `Fn(Num, Hud) -> Bool`, which the other language may call
//! as one of its own, or a class of the module or one it imports;
//! anything else, and an undeclared parameter or result, is `Dyn`.

use caribou::registry::TypeRef;
pub use wren_lift::sema::export::Export;

/// An export's parameter and result types as registry types.
pub trait ExportTypes {
    /// The type of the parameter at `index`, among `classes`.
    fn param(&self, index: usize, classes: &Classes<'_>) -> TypeRef;
    fn ret(&self, classes: &Classes<'_>) -> Option<TypeRef>;
}

impl ExportTypes for Export {
    fn param(&self, index: usize, classes: &Classes<'_>) -> TypeRef {
        self.params
            .get(index)
            .and_then(|p| p.ty.as_deref())
            .map_or(TypeRef::Dyn, |t| type_ref(t, classes))
    }

    fn ret(&self, classes: &Classes<'_>) -> Option<TypeRef> {
        self.ret.as_deref().map(|t| type_ref(t, classes))
    }
}

/// The index just past the `)` matching the `(` at `open`, if any.
fn close_of(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` split at the commas outside parentheses.
fn split_top(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The classes a type name in an export may name: the module's own, by
/// name, and the ones the module imports, by the name it imports them
/// as, each with the type name the registry knows the class by.
pub struct Classes<'a> {
    pub module: &'a str,
    pub own: &'a [String],
    pub imported: &'a [(String, String)],
}

impl Classes<'_> {
    /// The registry's name for the class `name` names here.
    pub fn type_name(&self, name: &str) -> Option<String> {
        if self.own.iter().any(|c| c == name) {
            return Some(format!("{}.{name}", self.module));
        }
        self.imported
            .iter()
            .find(|(local, _)| local == name)
            .map(|(_, type_name)| type_name.clone())
    }
}

/// A Wren type name as a registry type. `Fn(T, U) -> R` is a function of
/// that shape; `Fn` alone one of any.
pub fn type_ref(name: &str, classes: &Classes<'_>) -> TypeRef {
    let name = name.trim();
    if name.starts_with("Fn(")
        && let Some(close) = close_of(name, 2)
    {
        let inner = &name[3..close - 1];
        let params = if inner.trim().is_empty() {
            Vec::new()
        } else {
            split_top(inner)
                .into_iter()
                .map(|p| type_ref(p, classes))
                .collect()
        };
        let ret = match name[close..].trim().strip_prefix("->") {
            Some(r) => type_ref(r, classes),
            None => TypeRef::Dyn,
        };
        return TypeRef::Function {
            params,
            ret: Box::new(ret),
        };
    }
    match name {
        "Num" => TypeRef::Float,
        "Bool" => TypeRef::Bool,
        "String" => TypeRef::Str,
        "Null" => TypeRef::Void,
        "List" => TypeRef::Array(Box::new(TypeRef::Dyn)),
        "Fn" => TypeRef::Fun,
        // A typed array crosses as a buffer over its storage.
        "ByteArray" | "Int32Array" | "Float32Array" | "Float64Array" => TypeRef::Buffer,
        _ if let Some(type_name) = classes.type_name(name) => TypeRef::Object(type_name),
        _ => TypeRef::Dyn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_map_to_registry_types() {
        let own = vec!["Hud".to_owned()];
        let imported = vec![("Entity".to_owned(), "swarm:Entity.Entity".to_owned())];
        let classes = Classes {
            module: "hud",
            own: &own,
            imported: &imported,
        };
        let e = Export::parse("hit(n: Num, other: Hud) -> Bool").unwrap();
        assert_eq!(e.param(0, &classes), TypeRef::Float);
        assert_eq!(e.param(1, &classes), TypeRef::Object("hud.Hud".to_owned()));
        assert_eq!(e.param(2, &classes), TypeRef::Dyn);
        assert_eq!(e.ret(&classes), Some(TypeRef::Bool));
        // An imported class, by the name it is imported as.
        let e = Export::parse("new(e: Entity, i: Num)").unwrap();
        assert_eq!(
            e.param(0, &classes),
            TypeRef::Object("swarm:Entity.Entity".to_owned())
        );
        // A typed array is a buffer over its storage.
        let e = Export::parse("fill(into: Float32Array) -> ByteArray").unwrap();
        assert_eq!(e.param(0, &classes), TypeRef::Buffer);
        assert_eq!(e.ret(&classes), Some(TypeRef::Buffer));
        let e = Export::parse("adder() -> Fn(Num) -> Num").unwrap();
        assert_eq!(
            e.ret(&classes),
            Some(TypeRef::Function {
                params: vec![TypeRef::Float],
                ret: Box::new(TypeRef::Float)
            })
        );
        let e = Export::parse("each(f: Fn(Hud, Num), n: Num)").unwrap();
        assert_eq!(
            e.param(0, &classes),
            TypeRef::Function {
                params: vec![TypeRef::Object("hud.Hud".to_owned()), TypeRef::Float],
                ret: Box::new(TypeRef::Dyn)
            }
        );
        let e = Export::parse("done -> Fn()").unwrap();
        assert_eq!(
            e.ret(&classes),
            Some(TypeRef::Function {
                params: vec![],
                ret: Box::new(TypeRef::Dyn)
            })
        );
        let e = Export::parse("blank -> Null").unwrap();
        assert_eq!(e.ret(&classes), Some(TypeRef::Void));
        let e = Export::parse("f(_: Num, b)").unwrap();
        assert_eq!(e.param(1, &classes), TypeRef::Dyn);
    }
}
