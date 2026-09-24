//! Typed native binding declarations to Rust wrappers and `plugin!` exports.
//! Traits describe resource classes; structs describe plugin-owned records;
//! enums whose variants carry a type describe unions, which records set
//! through one setter per variant; `#[native(name)]` selects a backend
//! function. `#[idl("Name")]` imports enum values, namespace constants,
//! dictionary members or union alternatives from a vendored WebIDL source. This
//! does not infer native GPU semantics from WebIDL interfaces or generate a
//! language-specific heap layout.
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::{HashMap, HashSet};
use syn::ext::IdentExt;
use syn::{FnArg, GenericArgument, Item, PathArguments, ReturnType, TraitItem, Type};

fn error(message: impl std::fmt::Display) -> String {
    message.to_string()
}
fn idl_name(attrs: &[syn::Attribute]) -> Result<Option<String>, String> {
    attrs
        .iter()
        .find(|a| a.path().is_ident("idl"))
        .map(|a| {
            a.parse_args::<syn::LitStr>()
                .map(|s| s.value())
                .map_err(error)
        })
        .transpose()
}
/// A WebIDL member may be a Rust keyword (`type`); it stays itself as a raw
/// identifier, which `plugin!` exports without the `r#`.
fn ident(name: &str) -> Result<syn::Ident, String> {
    syn::parse_str(name).or_else(|e| {
        let raw = !matches!(name, "self" | "Self" | "super" | "crate" | "_")
            && syn::parse_str::<syn::Ident>(&format!("r#{name}")).is_ok();
        if raw {
            Ok(syn::Ident::new_raw(name, proc_macro2::Span::call_site()))
        } else {
            Err(error(e))
        }
    })
}
fn pascal(name: &str) -> String {
    let mut result = String::new();
    for word in name.split('-') {
        let mut chars = word.chars();
        if let Some(c) = chars.next() {
            result.extend(c.to_uppercase());
            result.extend(chars);
        }
    }
    if result.starts_with(|c: char| c.is_ascii_digit()) {
        result.insert(0, 'D');
    }
    result
}

/// Tokenize just enough WebIDL to extract enums and integer namespaces.
/// Comments and quoted braces never affect declaration boundaries.
fn tokens(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = ' ';
            let mut closed = false;
            for c in chars.by_ref() {
                if previous == '*' && c == '/' {
                    closed = true;
                    break;
                }
                previous = c;
            }
            if !closed {
                return Err("unterminated WebIDL comment".into());
            }
            continue;
        }
        let mut token = c.to_string();
        if c == '"' {
            let mut escaped = false;
            let mut closed = false;
            for c in chars.by_ref() {
                token.push(c);
                if c == '"' && !escaped {
                    closed = true;
                    break;
                }
                escaped = c == '\\' && !escaped;
            }
            if !closed {
                return Err("unterminated WebIDL string".into());
            }
        } else if c.is_ascii_alphanumeric() || c == '_' {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                token.push(chars.next().unwrap());
            }
        }
        out.push(token);
    }
    Ok(out)
}
fn body<'a>(tokens: &'a [String], kind: &str, name: &str) -> Result<&'a [String], String> {
    let matches: Vec<_> = tokens
        .windows(2)
        .enumerate()
        .filter(|(_, t)| t[0] == kind && t[1] == name)
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL {kind} {name}, found {}",
            matches.len()
        ));
    }
    let declaration = matches[0].0;
    let open = tokens[declaration + 2..]
        .iter()
        .position(|s| s == "{")
        .map(|i| declaration + 2 + i)
        .ok_or_else(|| format!("{kind} {name} has no body"))?;
    let mut depth = 0usize;
    for (offset, token) in tokens[open..].iter().enumerate() {
        match token.as_str() {
            "{" => depth += 1,
            "}" => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&tokens[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!("unclosed {name}"))
}

fn declaration_prefix<'a>(
    tokens: &'a [String],
    kind: &str,
    name: &str,
) -> Result<&'a [String], String> {
    let matches: Vec<_> = tokens
        .windows(2)
        .enumerate()
        .filter(|(_, t)| t[0] == kind && t[1] == name)
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL {kind} {name}, found {}",
            matches.len()
        ));
    }
    let start = matches[0].0 + 2;
    let end = tokens[start..]
        .iter()
        .position(|s| s == "{")
        .ok_or_else(|| format!("{kind} {name} has no body"))?;
    Ok(&tokens[start..start + end])
}

fn statements(tokens: &[String]) -> Vec<&[String]> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut brace = 0usize;
    let mut bracket = 0usize;
    for (i, token) in tokens.iter().enumerate() {
        match token.as_str() {
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "{" => brace += 1,
            "}" => brace = brace.saturating_sub(1),
            "[" => bracket += 1,
            "]" => bracket = bracket.saturating_sub(1),
            ";" if angle == 0 && paren == 0 && brace == 0 && bracket == 0 => {
                if start < i {
                    result.push(&tokens[start..i]);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    result
}

fn typedefs(tokens: &[String]) -> HashMap<String, Vec<String>> {
    statements(tokens)
        .into_iter()
        .filter_map(|statement| {
            let start = statement.iter().position(|token| token == "typedef")? + 1;
            let alias = statement.last()?.clone();
            Some((alias, statement[start..statement.len() - 1].to_vec()))
        })
        .collect()
}

fn strip_attributes(mut ty: &[String]) -> &[String] {
    while ty.first().is_some_and(|token| token == "[") {
        let mut depth = 0usize;
        let Some(end) = ty.iter().position(|token| {
            if token == "[" {
                depth += 1;
            } else if token == "]" {
                depth -= 1;
            }
            depth == 0
        }) else {
            break;
        };
        ty = &ty[end + 1..];
    }
    ty
}

fn idl_generic<'a>(tokens: &'a [String], name: &str) -> Option<Vec<&'a [String]>> {
    if tokens.len() < 4 || tokens[0] != name || tokens[1] != "<" || tokens.last()? != ">" {
        return None;
    }
    let inner = &tokens[2..tokens.len() - 1];
    let mut parts = Vec::new();
    let mut start = 0;
    let mut angle = 0usize;
    let mut paren = 0usize;
    for (i, token) in inner.iter().enumerate() {
        match token.as_str() {
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "," if angle == 0 && paren == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    Some(parts)
}

/// The alternatives of a parenthesised WebIDL union, `undefined` and `null`
/// left out; `None` for a type that is not a union.
fn union_alternatives(tokens: &[String]) -> Option<Vec<&[String]>> {
    if tokens.first()? != "(" || tokens.last()? != ")" {
        return None;
    }
    let inner = &tokens[1..tokens.len() - 1];
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, token) in inner.iter().enumerate() {
        match token.as_str() {
            "<" => angle += 1,
            ">" => angle = angle.saturating_sub(1),
            "(" => paren += 1,
            ")" => paren = paren.saturating_sub(1),
            "or" if angle == 0 && paren == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    Some(
        parts
            .into_iter()
            .filter(|part| *part != ["undefined"] && *part != ["null"])
            .collect(),
    )
}

fn idl_union_without_undefined(tokens: &[String]) -> Option<&[String]> {
    let concrete = union_alternatives(tokens)?;
    (concrete.len() == 1).then(|| concrete[0])
}

fn idl_type(
    tokens: &[String],
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
    resolving: &mut HashSet<String>,
) -> Result<Type, String> {
    let tokens = strip_attributes(tokens);
    if tokens.last().is_some_and(|token| token == "?") {
        let inner = idl_type(&tokens[..tokens.len() - 1], aliases, named, resolving)?;
        return Ok(syn::parse_quote!(Option<#inner>));
    }
    if let Some(inner) = idl_union_without_undefined(tokens) {
        let inner = idl_type(inner, aliases, named, resolving)?;
        return Ok(syn::parse_quote!(Option<#inner>));
    }
    if let Some(parts) = idl_generic(tokens, "sequence") {
        if parts.len() != 1 {
            return Err("WebIDL sequence needs one element type".into());
        }
        let inner = idl_type(parts[0], aliases, named, resolving)?;
        return Ok(syn::parse_quote!(Vec<#inner>));
    }
    if let Some(parts) = idl_generic(tokens, "record") {
        if parts.len() != 2 {
            return Err("WebIDL record needs key and value types".into());
        }
        let key = idl_type(parts[0], aliases, named, resolving)?;
        let mut value = idl_type(parts[1], aliases, named, resolving)?;
        if let Some(inner) = generic(&value, "Option") {
            value = inner;
        }
        return Ok(syn::parse_quote!(Map<#key, #value>));
    }
    if let Some(parts) = idl_generic(tokens, "Promise") {
        if parts.len() != 1 {
            return Err("WebIDL Promise needs one result type".into());
        }
        let mut inner = idl_type(parts[0], aliases, named, resolving)?;
        // A rejected future represents the WebIDL operation's absence/error
        // path. Caribou frontends therefore expose a nullable Promise result
        // as the same typed result as a non-null Promise.
        if let Some(value) = generic(&inner, "Option") {
            inner = value;
        }
        return Ok(syn::parse_quote!(Future<#inner>));
    }
    let spelling = tokens.join(" ");
    let primitive = match spelling.as_str() {
        "undefined" => Some(syn::parse_quote!(())),
        "boolean" => Some(syn::parse_quote!(bool)),
        "byte" | "octet" | "short" | "unsigned short" | "long" | "unsigned long" => {
            Some(syn::parse_quote!(i32))
        }
        "long long" | "unsigned long long" => Some(syn::parse_quote!(i64)),
        "float" => Some(syn::parse_quote!(f32)),
        "double" => Some(syn::parse_quote!(f64)),
        "DOMString" | "USVString" | "ByteString" => Some(syn::parse_quote!(Text)),
        _ => None,
    };
    if let Some(primitive) = primitive {
        return Ok(primitive);
    }
    if tokens.len() == 1 {
        let name = &tokens[0];
        if let Some(target) = named.get(name) {
            return Ok(target.clone());
        }
        if let Some(alias) = aliases.get(name) {
            if !resolving.insert(name.clone()) {
                return Err(format!("recursive WebIDL typedef {name}"));
            }
            let result = idl_type(alias, aliases, named, resolving);
            resolving.remove(name);
            return result;
        }
    }
    Err(format!("unsupported WebIDL type {spelling}"))
}

fn operation_return(
    tokens: &[String],
    source: &str,
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
) -> Result<Type, String> {
    let (interface, operation) = source
        .split_once('.')
        .ok_or_else(|| format!("WebIDL operation {source} must be Interface.method"))?;
    let matches: Vec<_> = statements(body(tokens, "interface", interface)?)
        .into_iter()
        .filter_map(|statement| {
            statement
                .windows(2)
                .position(|part| part[0] == operation && part[1] == "(")
                .map(|at| &statement[..at])
        })
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL operation {source}, found {}",
            matches.len()
        ));
    }
    idl_type(matches[0], aliases, named, &mut HashSet::new())
}

fn dictionary_fields(
    tokens: &[String],
    name: &str,
    aliases: &HashMap<String, Vec<String>>,
    named: &HashMap<String, Type>,
    overrides: &HashMap<String, Type>,
) -> Result<Vec<(syn::Ident, Type)>, String> {
    let mut fields = Vec::new();
    let prefix = declaration_prefix(tokens, "dictionary", name)?;
    if prefix.first().is_some_and(|token| token == ":") {
        let parent = prefix
            .get(1)
            .ok_or_else(|| format!("dictionary {name} has no parent name"))?;
        fields.extend(dictionary_fields(
            tokens, parent, aliases, named, overrides,
        )?);
    } else if !prefix.is_empty() {
        return Err(format!("unsupported dictionary declaration for {name}"));
    }
    for statement in statements(body(tokens, "dictionary", name)?) {
        let required = statement.first().is_some_and(|token| token == "required");
        let statement = if required { &statement[1..] } else { statement };
        let before_default = statement
            .iter()
            .position(|token| token == "=")
            .map_or(statement, |at| &statement[..at]);
        let (field, ty) = before_default
            .split_last()
            .ok_or_else(|| format!("empty member in dictionary {name}"))?;
        let field = ident(field)?;
        let mut ty = if let Some(override_type) = overrides.get(&field.to_string()) {
            override_type.clone()
        } else {
            idl_type(ty, aliases, named, &mut HashSet::new())?
        };
        if !required && generic(&ty, "Vec").is_none() && generic_pair(&ty, "Map").is_none() {
            ty = syn::parse_quote!(Option<#ty>);
        }
        fields.push((field, ty));
    }
    Ok(fields)
}
/// An integer literal discriminant, possibly negative.
fn discriminant(expr: &syn::Expr) -> Option<i32> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(value),
            ..
        }) => value.base10_parse().ok(),
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => discriminant(expr)?.checked_neg(),
        syn::Expr::Paren(inner) => discriminant(&inner.expr),
        _ => None,
    }
}

fn enum_values(tokens: &[String], name: &str) -> Result<Vec<String>, String> {
    let body = body(tokens, "enum", name)?;
    let mut values = Vec::new();
    for (i, token) in body.iter().enumerate() {
        if i % 2 == 1 {
            if token != "," {
                return Err(format!("expected comma in {name}"));
            }
        } else {
            values.push(syn::parse_str::<syn::LitStr>(token).map_err(error)?.value());
        }
    }
    if values.is_empty() {
        return Err(format!("empty enum {name}"));
    }
    Ok(values)
}

/// Names of readonly attributes on an interface. Finite interface catalogs
/// such as `GPUSupportedLimits` can therefore generate a Caribou enum without
/// copying their member list into the declaration.
fn readonly_attribute_names(tokens: &[String], name: &str) -> Result<Vec<String>, String> {
    let body = body(tokens, "interface", name)?;
    let mut values = Vec::new();
    for statement in body.split(|token| token == ";").filter(|s| !s.is_empty()) {
        if statement.first().is_some_and(|token| token == "readonly")
            && statement.get(1).is_some_and(|token| token == "attribute")
        {
            values.push(
                statement
                    .last()
                    .ok_or_else(|| format!("attribute without a name in {name}"))?
                    .clone(),
            );
        }
    }
    if values.is_empty() {
        return Err(format!("interface {name} has no readonly attributes"));
    }
    Ok(values)
}
fn generic(ty: &Type, name: &str) -> Option<Type> {
    let Type::Path(p) = ty else { return None };
    let segment = p.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    if args.args.len() != 1 {
        return None;
    }
    match args.args.first()? {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    }
}
fn generic_pair(ty: &Type, name: &str) -> Option<(Type, Type)> {
    let Type::Path(p) = ty else { return None };
    let segment = p.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut types = args.args.iter().filter_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty.clone()),
        _ => None,
    });
    let pair = (types.next()?, types.next()?);
    types.next().is_none().then_some(pair)
}
fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(p) = ty else { return None };
    p.path.get_ident().map(ToString::to_string)
}
fn scalar(ty: &Type) -> bool {
    if generic(ty, "Future").is_some() {
        return true;
    }
    type_name(ty).is_some_and(|s| {
        matches!(
            s.as_str(),
            "i32" | "u32" | "i64" | "f32" | "f64" | "bool" | "Text" | "Buffer" | "Future"
        )
    })
}

fn stored_value(
    ty: &Type,
    resources: &HashSet<String>,
    records: &HashSet<String>,
) -> Result<(TokenStream, TokenStream, TokenStream), String> {
    if let Some(enumeration) = generic(ty, "Enum") {
        return Ok((
            quote!(i32),
            quote!(Enum<#enumeration>),
            quote!(value.get().native()),
        ));
    }
    if scalar(ty) {
        if type_name(ty).as_deref() == Some("Text") {
            return Ok((
                quote!(caribou_abi::Rooted<Text>),
                quote!(Text),
                quote!(caribou_abi::Rooted::new(value)),
            ));
        }
        if type_name(ty).as_deref() == Some("Buffer") {
            return Ok((
                quote!(caribou_abi::Rooted<Buffer>),
                quote!(Buffer),
                quote!(caribou_abi::Rooted::new(value)),
            ));
        }
        return Ok((quote!(#ty), quote!(#ty), quote!(value)));
    }
    let name = type_name(ty).ok_or("record fields need named types")?;
    let ident = ident(&name)?;
    if resources.contains(&name) {
        Ok((quote!(i32), quote!(&#ident), quote!(value.handle)))
    } else if records.contains(&name) {
        Ok((quote!(#ident), quote!(&#ident), quote!(value.clone())))
    } else {
        Err(format!("unsupported record field type {name}"))
    }
}

/// Emit a self-contained set of resource wrappers, schemas and one plugin
/// table. Backends implement the selected functions with integer handles;
/// the generated ABI uses typed native objects, enums, Text and Buffer.
pub fn generate(namespace: &str, declaration: &str, webidl: &str) -> Result<String, String> {
    ident(namespace)?;
    let file = syn::parse_file(declaration).map_err(error)?;
    let idl = tokens(webidl)?;
    let aliases = typedefs(&idl);
    let classes: HashSet<_> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Trait(t) => Some(t.ident.to_string()),
            Item::Struct(s) => Some(s.ident.to_string()),
            _ => None,
        })
        .collect();
    let resources: HashSet<_> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Trait(t) => Some(t.ident.to_string()),
            _ => None,
        })
        .collect();
    let records: HashSet<_> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Struct(s) => Some(s.ident.to_string()),
            _ => None,
        })
        .collect();
    // An enum whose variants carry a value declares a union: a record field
    // of that type takes one setter per alternative instead of a dynamic
    // value. Each variant holds exactly one declared type.
    let mut unions: HashMap<String, Vec<(syn::Ident, Type)>> = HashMap::new();
    for item in &file.items {
        let Item::Enum(e) = item else { continue };
        if e.variants
            .iter()
            .all(|v| matches!(v.fields, syn::Fields::Unit))
        {
            continue;
        }
        let mut alternatives = Vec::new();
        for v in &e.variants {
            let syn::Fields::Unnamed(fields) = &v.fields else {
                return Err(format!(
                    "union {}::{} needs one unnamed type",
                    e.ident, v.ident
                ));
            };
            if fields.unnamed.len() != 1 || v.discriminant.is_some() {
                return Err(format!(
                    "union {}::{} needs one unnamed type",
                    e.ident, v.ident
                ));
            }
            alternatives.push((v.ident.clone(), fields.unnamed[0].ty.clone()));
        }
        unions.insert(e.ident.to_string(), alternatives);
    }
    let enums: HashSet<_> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Enum(e) if !unions.contains_key(&e.ident.to_string()) => {
                Some(e.ident.to_string())
            }
            _ => None,
        })
        .collect();
    let mut idl_types = HashMap::new();
    for item in &file.items {
        let (attrs, ty): (&[syn::Attribute], Type) = match item {
            Item::Enum(item) if unions.contains_key(&item.ident.to_string()) => {
                let local = &item.ident;
                (item.attrs.as_slice(), syn::parse_quote!(#local))
            }
            Item::Enum(item) => {
                let local = &item.ident;
                (item.attrs.as_slice(), syn::parse_quote!(Enum<#local>))
            }
            Item::Struct(item) => {
                let local = &item.ident;
                (item.attrs.as_slice(), syn::parse_quote!(#local))
            }
            Item::Trait(item) => {
                let local = &item.ident;
                (item.attrs.as_slice(), syn::parse_quote!(#local))
            }
            _ => continue,
        };
        if let Some(source) = idl_name(attrs)?
            && idl_types.insert(source.clone(), ty).is_some()
        {
            return Err(format!("WebIDL type {source} is imported more than once"));
        }
    }
    let mut names = HashSet::new();
    let mut output = TokenStream::new();
    let mut exports = TokenStream::new();
    for item in file.items {
        let name = match &item {
            Item::Enum(e) => &e.ident,
            Item::Trait(t) => &t.ident,
            Item::Struct(s) => &s.ident,
            Item::Mod(m) => &m.ident,
            _ => {
                return Err(
                    "declarations support enums, records, resource traits and constant modules"
                        .into(),
                );
            }
        };
        if !names.insert(name.to_string()) {
            return Err(format!("duplicate export {name}"));
        }
        match item {
            Item::Enum(e) if unions.contains_key(&e.ident.to_string()) => {
                let name = &e.ident;
                let alternatives = &unions[&name.to_string()];
                if let Some(source) = idl_name(&e.attrs)? {
                    // A declared subset of the WebIDL union: each variant
                    // must be one of its alternatives.
                    let union = aliases
                        .get(&source)
                        .map(|alias| strip_attributes(alias))
                        .and_then(union_alternatives)
                        .ok_or_else(|| format!("{source} is not a WebIDL union typedef"))?;
                    let mapped: Vec<String> = union
                        .into_iter()
                        .filter_map(|alternative| {
                            idl_type(alternative, &aliases, &idl_types, &mut HashSet::new()).ok()
                        })
                        .map(|ty| quote!(#ty).to_string())
                        .collect();
                    for (variant, ty) in alternatives {
                        if !mapped.contains(&quote!(#ty).to_string()) {
                            return Err(format!(
                                "{name}::{variant} is not an alternative of {source}"
                            ));
                        }
                    }
                }
                let mut stored_variants = Vec::new();
                for (variant, ty) in alternatives {
                    if let Some(enumeration) = generic(ty, "Enum")
                        && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                    {
                        return Err(format!("unknown enum in {name}::{variant}"));
                    }
                    let (stored, _, _) = stored_value(ty, &resources, &records)?;
                    stored_variants.push(quote!(#variant(#stored)));
                }
                output.extend(quote! {
                    #[derive(Clone)]
                    pub enum #name { #(#stored_variants),* }
                });
            }
            Item::Enum(e) => {
                let name = &e.ident;
                let schema = format!("{namespace}.{name}");
                let source = idl_name(&e.attrs)?;
                // Native codes are evaluated here, so the generated code
                // matches on plain integer literals.
                let variants: Vec<(syn::Ident, i32)> =
                    if let Some(source) = source.filter(|_| e.variants.is_empty()) {
                        enum_values(&idl, &source)
                            .or_else(|_| readonly_attribute_names(&idl, &source))?
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let code = i32::try_from(i).map_err(error)?;
                                Ok((ident(&pascal(&v))?, code))
                            })
                            .collect::<Result<_, String>>()?
                    } else {
                        let mut next = 0i32;
                        let mut values = Vec::new();
                        for v in &e.variants {
                            if !matches!(v.fields, syn::Fields::Unit) {
                                return Err("native enums must be fieldless".into());
                            }
                            let value = match &v.discriminant {
                                Some((_, expr)) => discriminant(expr).ok_or_else(|| {
                                    format!("{name}.{} needs an integer literal", v.ident)
                                })?,
                                None => next,
                            };
                            next = value
                                .checked_add(1)
                                .ok_or_else(|| format!("{name} overflows i32"))?;
                            values.push((v.ident.clone(), value));
                        }
                        values
                    };
                if variants.is_empty() {
                    return Err(format!("empty enum {name}"));
                }
                let mut seen = HashSet::new();
                for (v, _) in &variants {
                    if !seen.insert(v.to_string()) {
                        return Err(format!("duplicate variant {name}.{v}"));
                    }
                }
                let ids: Vec<_> = variants.iter().map(|(v, _)| v).collect();
                let values: Vec<_> = variants
                    .iter()
                    .map(|(_, value)| proc_macro2::Literal::i32_unsuffixed(*value))
                    .collect();
                let (first, rest) = ids.split_first().expect("a non-empty enum");
                output.extend(quote! {
                    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, caribou_abi::PluginEnum)]
                    #[caribou(name = #schema)]
                    pub enum #name { #[default] #first, #(#rest),* }
                    impl #name {
                        pub fn native(self) -> i32 { match self { #(Self::#ids => #values),* } }
                        pub fn from_native(value: i32) -> Option<Self> {
                            match value {
                                #(#values => Some(Self::#ids),)*
                                _ => None,
                            }
                        }
                    }
                });
                exports.extend(quote!(enum #name;));
            }
            Item::Mod(m) => {
                let name = &m.ident;
                let source =
                    idl_name(&m.attrs)?.ok_or("constant modules require #[idl(\"Namespace\")]")?;
                let body = body(&idl, "namespace", &source)?;
                let mut methods = TokenStream::new();
                let mut signatures = TokenStream::new();
                for statement in body.split(|s| s == ";").filter(|s| !s.is_empty()) {
                    if statement.len() != 5 || statement[0] != "const" || statement[3] != "=" {
                        return Err(format!("unsupported constant in {source}"));
                    }
                    let field = ident(&statement[2])?;
                    let value: syn::LitInt = syn::parse_str(&statement[4]).map_err(error)?;
                    methods.extend(quote!(pub extern "C" fn #field() -> i32 { #value }));
                    signatures.extend(quote!(fn #field() -> i32;));
                }
                output.extend(quote!(pub struct #name; impl #name { #methods }));
                exports.extend(quote!(class #name { #signatures }));
            }
            Item::Struct(s) => {
                let class = &s.ident;
                if !s.generics.params.is_empty() {
                    return Err("records cannot be generic".into());
                }
                let syn::Fields::Named(fields) = &s.fields else {
                    return Err("records need named fields".into());
                };
                let imported = idl_name(&s.attrs)?;
                let explicit_fields: Vec<(syn::Ident, Type)> = fields
                    .named
                    .iter()
                    .map(|field| {
                        if !field.attrs.is_empty() {
                            return Err(format!(
                                "record field attributes are not supported on {class}"
                            ));
                        }
                        Ok((field.ident.clone().expect("named field"), field.ty.clone()))
                    })
                    .collect::<Result<_, String>>()?;
                let declared_fields: Vec<(syn::Ident, Type)> = if let Some(source) = imported {
                    let overrides: HashMap<_, _> = explicit_fields
                        .iter()
                        .map(|(name, ty)| (name.to_string(), ty.clone()))
                        .collect();
                    let imported =
                        dictionary_fields(&idl, &source, &aliases, &idl_types, &overrides)?;
                    for name in overrides.keys() {
                        if !imported.iter().any(|(field, _)| field == name.as_str()) {
                            return Err(format!(
                                "{class}.{name} does not override a member of {source}"
                            ));
                        }
                    }
                    imported
                } else {
                    explicit_fields
                };
                let mut stored_fields = TokenStream::new();
                let mut required_params = Vec::new();
                let mut required_types = Vec::new();
                let mut required_values = Vec::new();
                let mut initial_values = Vec::new();
                let mut methods = TokenStream::new();
                let mut signatures = TokenStream::new();
                let mut field_names = HashSet::new();
                let mut method_names = HashSet::from(["new".to_owned()]);
                for (field_name, field_ty) in declared_fields {
                    if !field_names.insert(field_name.to_string()) {
                        return Err(format!("duplicate field {class}.{field_name}"));
                    }
                    if let Some((key_ty, value_ty)) = generic_pair(&field_ty, "Map") {
                        for ty in [&key_ty, &value_ty] {
                            if type_name(ty).is_some_and(|name| unions.contains_key(&name)) {
                                return Err(format!("{class}.{field_name} maps a union"));
                            }
                            if let Some(enumeration) = generic(ty, "Enum")
                                && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                            {
                                return Err(format!("unknown enum in {class}.{field_name}"));
                            }
                        }
                        let (stored_key, parameter_key, convert_key) =
                            stored_value(&key_ty, &resources, &records)?;
                        let (stored_value_ty, parameter_value, convert_value) =
                            stored_value(&value_ty, &resources, &records)?;
                        stored_fields.extend(
                            quote!(pub(crate) #field_name: Vec<(#stored_key, #stored_value_ty)>,),
                        );
                        initial_values.push(quote!(#field_name: Vec::new()));
                        let add =
                            ident(&format!("add{}", pascal(&field_name.unraw().to_string())))?;
                        if !method_names.insert(add.to_string()) {
                            return Err(format!("generated method {class}.{add} is duplicated"));
                        }
                        methods.extend(quote! {
                            pub extern "C" fn #add(
                                this: &mut #class,
                                key: #parameter_key,
                                value: #parameter_value,
                            ) {
                                let key = { let value = key; #convert_key };
                                let value = { #convert_value };
                                this.#field_name.push((key, value));
                            }
                        });
                        signatures.extend(
                            quote!(fn #add(&mut #class, #parameter_key, #parameter_value);),
                        );
                        continue;
                    }
                    let (container, value_ty) = if let Some(inner) = generic(&field_ty, "Option") {
                        ("option", inner)
                    } else if let Some(inner) = generic(&field_ty, "Vec") {
                        ("sequence", inner)
                    } else {
                        ("required", field_ty)
                    };
                    let lowered_ty = if container == "sequence" {
                        generic(&value_ty, "Option").unwrap_or_else(|| value_ty.clone())
                    } else {
                        value_ty.clone()
                    };
                    let member = field_name.unraw().to_string();
                    if let Some(alternatives) =
                        type_name(&lowered_ty).and_then(|name| unions.get(&name))
                    {
                        // One setter per alternative. A required union is
                        // not a constructor argument; the backend checks it
                        // was set.
                        let union = &lowered_ty;
                        let sequence = container == "sequence";
                        if sequence && generic(&value_ty, "Option").is_some() {
                            return Err(format!("{class}.{field_name} holds nullable unions"));
                        }
                        if sequence {
                            stored_fields.extend(quote!(pub(crate) #field_name: Vec<#union>,));
                            initial_values.push(quote!(#field_name: Vec::new()));
                        } else {
                            stored_fields.extend(quote!(pub(crate) #field_name: Option<#union>,));
                            initial_values.push(quote!(#field_name: None));
                        }
                        for (variant, ty) in alternatives {
                            let (_, parameter, convert) = stored_value(ty, &resources, &records)?;
                            let setter = if sequence {
                                ident(&format!("add{}{variant}", pascal(&member)))?
                            } else {
                                ident(&format!("{member}{variant}"))?
                            };
                            if !method_names.insert(setter.to_string()) {
                                return Err(format!(
                                    "generated method {class}.{setter} is duplicated"
                                ));
                            }
                            let store = if sequence {
                                quote!(this.#field_name.push(#union::#variant(#convert)))
                            } else {
                                quote!(this.#field_name = Some(#union::#variant(#convert)))
                            };
                            methods.extend(quote! {
                                pub extern "C" fn #setter(this: &mut #class, value: #parameter) {
                                    #store;
                                }
                            });
                            signatures.extend(quote!(fn #setter(&mut #class, #parameter);));
                        }
                        continue;
                    }
                    if let Some(enumeration) = generic(&lowered_ty, "Enum")
                        && !enums.contains(&type_name(&enumeration).unwrap_or_default())
                    {
                        return Err(format!("unknown enum in {class}.{field_name}"));
                    }
                    let (stored, parameter, convert) =
                        stored_value(&lowered_ty, &resources, &records)?;
                    match container {
                        "required" => {
                            stored_fields.extend(quote!(pub(crate) #field_name: #stored,));
                            required_params.push(quote!(#field_name: #parameter));
                            required_types.push(quote!(#parameter));
                            if convert.to_string() == "value" {
                                required_values.push(quote!(#field_name));
                            } else {
                                required_values.push(
                                    quote!(#field_name: { let value = #field_name; #convert }),
                                );
                            }
                        }
                        "option" => {
                            if !method_names.insert(member.clone()) {
                                return Err(format!(
                                    "generated method {class}.{field_name} is duplicated"
                                ));
                            }
                            stored_fields.extend(quote!(pub(crate) #field_name: Option<#stored>,));
                            initial_values.push(quote!(#field_name: None));
                            methods.extend(quote! {
                                pub extern "C" fn #field_name(this: &mut #class, value: #parameter) {
                                    this.#field_name = Some(#convert);
                                }
                            });
                            signatures.extend(quote!(fn #field_name(&mut #class, #parameter);));
                        }
                        "sequence" => {
                            let nullable = generic(&value_ty, "Option");
                            if nullable.is_some() {
                                stored_fields
                                    .extend(quote!(pub(crate) #field_name: Vec<Option<#stored>>,));
                            } else {
                                stored_fields.extend(quote!(pub(crate) #field_name: Vec<#stored>,));
                            }
                            initial_values.push(quote!(#field_name: Vec::new()));
                            let add = ident(&format!("add{}", pascal(&member)))?;
                            if !method_names.insert(add.to_string()) {
                                return Err(format!(
                                    "generated method {class}.{add} is duplicated"
                                ));
                            }
                            if nullable.is_some() {
                                let add_null = ident(&format!("{add}Null"))?;
                                if !method_names.insert(add_null.to_string()) {
                                    return Err(format!(
                                        "generated method {class}.{add_null} is duplicated"
                                    ));
                                }
                                methods.extend(quote! {
                                    pub extern "C" fn #add(this: &mut #class, value: #parameter) {
                                        this.#field_name.push(Some(#convert));
                                    }
                                    pub extern "C" fn #add_null(this: &mut #class) {
                                        this.#field_name.push(None);
                                    }
                                });
                                signatures.extend(quote! {
                                    fn #add(&mut #class, #parameter);
                                    fn #add_null(&mut #class);
                                });
                                continue;
                            }
                            methods.extend(quote! {
                                pub extern "C" fn #add(this: &mut #class, value: #parameter) {
                                    this.#field_name.push(#convert);
                                }
                            });
                            signatures.extend(quote!(fn #add(&mut #class, #parameter);));
                        }
                        _ => unreachable!(),
                    }
                }
                methods.extend(quote! {
                    pub extern "C" fn new(#(#required_params),*) -> Box<#class> {
                        Box::new(#class { #(#required_values,)* #(#initial_values,)* })
                    }
                });
                signatures = quote!(fn new(#(#required_types),*) -> Box<#class>; #signatures);
                output.extend(quote! {
                    #[derive(Clone)]
                    pub struct #class { #stored_fields }
                    impl #class { #methods }
                });
                exports.extend(quote!(class #class { #signatures }));
            }
            Item::Trait(t) => {
                let class = &t.ident;
                if !t.generics.params.is_empty() || !t.supertraits.is_empty() {
                    return Err("resource traits cannot be generic or inherit".into());
                }
                let mut methods = TokenStream::new();
                let mut signatures = TokenStream::new();
                let mut names = HashSet::new();
                for method in t.items {
                    let TraitItem::Fn(f) = method else {
                        return Err("resources contain only methods".into());
                    };
                    let name = &f.sig.ident;
                    if !names.insert(name.to_string()) {
                        return Err(format!("duplicate method {class}.{name}"));
                    }
                    if f.default.is_some()
                        || f.sig.asyncness.is_some()
                        || f.sig.unsafety.is_some()
                        || !f.sig.generics.params.is_empty()
                        || f.sig.variadic.is_some()
                    {
                        return Err(format!("unsupported signature {class}.{name}"));
                    }
                    let native = f
                        .attrs
                        .iter()
                        .find(|a| a.path().is_ident("native"))
                        .ok_or_else(|| format!("{class}.{name} needs #[native(function)]"))?
                        .parse_args::<syn::Ident>()
                        .map_err(error)?;
                    if let Some(source) = idl_name(&f.attrs)? {
                        let imported = operation_return(&idl, &source, &aliases, &idl_types)?;
                        let declared = match &f.sig.output {
                            ReturnType::Default => syn::parse_quote!(()),
                            ReturnType::Type(_, ty) => (**ty).clone(),
                        };
                        if quote!(#imported).to_string() != quote!(#declared).to_string() {
                            return Err(format!(
                                "{class}.{name} returns {}, but {source} maps to {}",
                                quote!(#declared),
                                quote!(#imported)
                            ));
                        }
                    }
                    let mut params = Vec::new();
                    let mut types = Vec::new();
                    let mut args = Vec::new();
                    for (i, arg) in f.sig.inputs.iter().enumerate() {
                        let FnArg::Typed(arg) = arg else {
                            return Err("use an explicit this: &Class receiver".into());
                        };
                        let syn::Pat::Ident(pat) = &*arg.pat else {
                            return Err("arguments need simple names".into());
                        };
                        let param = &pat.ident;
                        let ty = &arg.ty;
                        let value = if let Type::Reference(r) = &**ty {
                            let target = type_name(&r.elem).ok_or("invalid object reference")?;
                            if !classes.contains(&target) {
                                return Err(format!("unknown resource {target}"));
                            }
                            if i == 0 && *class != target {
                                return Err(format!(
                                    "first object parameter must be the {class} receiver"
                                ));
                            }
                            if resources.contains(&target) {
                                quote!(#param.handle)
                            } else {
                                quote!(#param)
                            }
                        } else if let Some(e) = generic(ty, "Enum") {
                            if !enums.contains(&type_name(&e).unwrap_or_default()) {
                                return Err("unknown enum".into());
                            }
                            quote!(#param.get().native())
                        } else if scalar(ty) {
                            quote!(#param)
                        } else {
                            return Err(format!("unsupported argument type in {class}.{name}"));
                        };
                        params.push(quote!(#param: #ty));
                        types.push(quote!(#ty));
                        args.push(value);
                    }
                    let (return_type, convert, fallback) = match &f.sig.output {
                        ReturnType::Default => (quote!(), quote!(value), quote!(())),
                        ReturnType::Type(_, ty) => {
                            let (convert, fallback) = if let Some(target) = generic(ty, "Box") {
                                if !resources.contains(&type_name(&target).unwrap_or_default()) {
                                    return Err("unknown returned resource".into());
                                }
                                (quote!(Box::new(#target { handle: value })), quote!(0))
                            } else if let Some(target) = generic(ty, "Enum") {
                                if !enums.contains(&type_name(&target).unwrap_or_default()) {
                                    return Err("unknown returned enum".into());
                                }
                                (
                                    quote! { match #target::from_native(value) {
                                        Some(value) => value.into(),
                                        None => { caribou_abi::host::raise(caribou_abi::ErrorKind::Type, "native enum value is not declared"); #target::default().into() }
                                    } },
                                    quote!(#target::default().native()),
                                )
                            } else if scalar(ty) {
                                let fallback = match type_name(ty).as_deref() {
                                    Some("Text") => quote!(Text::NULL),
                                    Some("Buffer") => quote!(Buffer::NULL),
                                    Some("Future") => quote!(Future::NULL),
                                    _ => quote!(Default::default()),
                                };
                                let fallback = if generic(ty, "Future").is_some() {
                                    quote!(Future::NULL)
                                } else {
                                    fallback
                                };
                                (quote!(value), fallback)
                            } else {
                                return Err("unsupported return type".into());
                            };
                            (quote!(-> #ty), convert, fallback)
                        }
                    };
                    // A panic in the backend becomes a runtime error in the
                    // caller's language, and the fallback is returned.
                    let call = quote! {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                            backend::#native(#(#args),*)
                        }))
                    };
                    let raise = quote! {
                        let message = error.downcast_ref::<String>().map(String::as_str)
                            .or_else(|| error.downcast_ref::<&str>().copied())
                            .unwrap_or("native backend panicked");
                        caribou_abi::host::raise(caribou_abi::ErrorKind::Runtime, message);
                    };
                    let body = if return_type.is_empty() {
                        quote!(if let Err(error) = #call { #raise })
                    } else if convert.to_string() == "value" {
                        quote! {
                            match #call {
                                Ok(value) => value,
                                Err(error) => { #raise #fallback }
                            }
                        }
                    } else {
                        quote! {
                            let value = match #call {
                                Ok(value) => value,
                                Err(error) => { #raise #fallback }
                            };
                            #convert
                        }
                    };
                    methods.extend(quote! {
                        pub extern "C" fn #name(#(#params),*) #return_type { #body }
                    });
                    signatures.extend(quote!(fn #name(#(#types),*) #return_type;));
                }
                output.extend(quote!(#[derive(Default)] pub struct #class { pub(crate) handle: i32 } impl #class { #methods }));
                exports.extend(quote!(class #class { #signatures }));
            }
            _ => unreachable!(),
        }
    }
    Ok(quote!(#output caribou_abi::plugin! { name: #namespace; #exports }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webidl_comments_and_spacing_do_not_change_enum_values() {
        let idl = tokens(
            r#"// enum E { "wrong" };
          enum /* { } */ E { "one-minus-src", // comment with "quotes"
            "two", };
          enum Elsewhere { "ignored" };
        "#,
        )
        .unwrap();
        assert_eq!(enum_values(&idl, "E").unwrap(), ["one-minus-src", "two"]);
        assert_eq!(pascal("one-minus-src"), "OneMinusSrc");
        assert!(enum_values(&idl, "Missing").is_err());
        assert!(tokens("/* unterminated").is_err());
    }
    #[test]
    fn readonly_interface_attributes_can_generate_a_catalog_enum() {
        let generated = generate(
            "gpu",
            r#"#[idl("GPUSupportedLimits")] enum Limit {}"#,
            "interface GPUSupportedLimits { readonly attribute unsigned long maxTextureDimension1D; readonly attribute unsigned long long maxBufferSize; };",
        )
        .unwrap();
        assert!(generated.contains("MaxTextureDimension1D"));
        assert!(generated.contains("MaxBufferSize"));
    }
    #[test]
    fn generated_code_contains_typed_objects_and_no_foreign_string_abi() {
        let generated = generate("gpu", r#"
          #[idl("Power")] enum Power {}
          #[idl("Usage")] mod Usage {}
          trait Device {
            #[native(create)] fn new() -> Box<Device>;
            #[native(shader)] fn shader(this: &Device, source: Text, data: Buffer, power: Enum<Power>) -> Box<Shader>;
          }
          trait Shader { #[native(name)] fn name(this: &Shader) -> Text; }
          trait Work { #[native(done)] fn done(this: &Work) -> Future; }
        "#, r#"enum Power { "low-power", "high-performance" }; namespace Usage { const Flags COPY = 0x4; };"#).unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("gpu.Power"));
        assert!(generated.contains(
            "backend :: shader (this . handle , source , data , power . get () . native ())"
        ));
        assert!(generated.contains("Box :: new (Shader { handle : value })"));
        assert!(
            generated.contains(
                "fn shader (& Device , Text , Buffer , Enum < Power >) -> Box < Shader >"
            )
        );
        assert!(!generated.contains("wgpu.Power"));
        assert!(generated.contains("fn done (& Work) -> Future"));
    }
    #[test]
    fn promise_operations_map_to_the_shared_future_carrier() {
        let generated = generate(
            "gpu",
            r#"
              trait Queue {
                #[native(done)]
                #[idl("GPUQueue.onSubmittedWorkDone")]
                fn done(this: &Queue) -> Future<()>;
              }
            "#,
            "interface GPUQueue { Promise<undefined> onSubmittedWorkDone(); };",
        )
        .unwrap();
        assert!(generated.contains("fn done (& Queue) -> Future < () >"));

        let wrong = generate(
            "gpu",
            r#"
              trait Queue {
                #[native(done)]
                #[idl("GPUQueue.onSubmittedWorkDone")]
                fn done(this: &Queue) -> i32;
              }
            "#,
            "interface GPUQueue { Promise<undefined> onSubmittedWorkDone(); };",
        );
        assert!(wrong.is_err());
    }
    #[test]
    fn records_generate_required_optional_and_sequence_fields() {
        let generated = generate(
            "gpu",
            r#"
              enum Format { Rgba }
              struct Entry { slot: i32 }
              struct Descriptor {
                size: i64,
                label: Option<Text>,
                format: Option<Enum<Format>>,
                buffer: Option<BufferResource>,
                entries: Vec<Entry>,
              }
              trait BufferResource {}
              trait Device {
                #[native(create)] fn create(this: &Device, descriptor: &Descriptor);
              }
            "#,
            "",
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("pub (crate) size : i64"));
        assert!(
            generated.contains("pub (crate) label : Option < caribou_abi :: Rooted < Text > >")
        );
        assert!(generated.contains("pub (crate) format : Option < i32 >"));
        assert!(generated.contains("pub (crate) buffer : Option < i32 >"));
        assert!(generated.contains("pub (crate) entries : Vec < Entry >"));
        assert!(generated.contains("fn new (size : i64) -> Box < Descriptor >"));
        assert!(generated.contains("fn label (& mut Descriptor , Text)"));
        assert!(generated.contains("fn addEntries (& mut Descriptor , & Entry)"));
        assert!(generated.contains("backend :: create (this . handle , descriptor)"));
    }
    #[test]
    fn idl_records_import_inheritance_typedefs_defaults_and_sequences() {
        let generated = generate(
            "gpu",
            r#"
              #[idl("GPUFormat")] enum Format {}
              #[idl("GPUExtent")] struct Extent {}
              #[idl("GPUDescriptor")] struct Descriptor { extent: Extent }
            "#,
            r#"
              enum GPUFormat { "rgba", "depth" };
              dictionary GPUBase { DOMString label = ""; };
              typedef [EnforceRange] unsigned long long GPUSize;
              dictionary GPUExtent { required unsigned long width; };
              typedef (sequence<unsigned long> or GPUExtent) GPUExtentUnion;
              dictionary GPUDescriptor : GPUBase {
                required GPUSize size;
                required GPUExtentUnion extent;
                boolean enabled = false;
                sequence<GPUFormat> formats = [];
                record<DOMString, (GPUSize or undefined)> limits = {};
                sequence<GPUExtent?> layouts = [];
              };
            "#,
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(
            generated.contains("pub (crate) label : Option < caribou_abi :: Rooted < Text > >")
        );
        assert!(generated.contains("pub (crate) size : i64"));
        assert!(generated.contains("pub (crate) extent : Extent"));
        assert!(generated.contains("pub (crate) enabled : Option < bool >"));
        assert!(generated.contains("pub (crate) formats : Vec < i32 >"));
        assert!(
            generated
                .contains("pub (crate) limits : Vec < (caribou_abi :: Rooted < Text > , i64) >")
        );
        assert!(generated.contains("pub (crate) layouts : Vec < Option < Extent >>"));
        assert!(generated.contains("fn new (i64 , & Extent) -> Box < Descriptor >"));
        assert!(generated.contains("fn addFormats (& mut Descriptor , Enum < Format >)"));
        assert!(generated.contains("fn addLimits (& mut Descriptor , Text , i64)"));
        assert!(generated.contains("fn addLayouts (& mut Descriptor , & Extent)"));
        assert!(generated.contains("fn addLayoutsNull (& mut Descriptor)"));
    }
    #[test]
    fn keyword_members_keep_their_webidl_names() {
        let generated = generate(
            "gpu",
            r#"
              #[idl("GPUBindingType")] enum BindingType {}
              #[idl("GPULayout")] struct Layout {}
            "#,
            r#"
              enum GPUBindingType { "uniform", "storage" };
              dictionary GPULayout { GPUBindingType type = "uniform"; sequence<long> match = []; };
            "#,
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains("pub (crate) r#type : Option < i32 >"));
        assert!(generated.contains("fn r#type (& mut Layout , Enum < BindingType >)"));
        assert!(generated.contains("fn addMatch (& mut Layout , i32)"));
    }
    #[test]
    fn declared_unions_take_one_setter_per_alternative() {
        let idl = r#"
          typedef (GPUSampler or GPUBuffer or GPUBufferBinding or GPUExternalTexture) GPUResource;
          dictionary GPUBufferBinding { required GPUBuffer buffer; unsigned long long size; };
          dictionary GPUEntry { required unsigned long binding; required GPUResource resource; };
          dictionary GPUGroup { sequence<GPUResource> extras = []; };
        "#;
        let generated = generate(
            "gpu",
            r#"
              #[idl("GPUSampler")] trait Sampler {}
              #[idl("GPUBuffer")] trait GpuBuffer {}
              #[idl("GPUBufferBinding")] struct BufferBinding {}
              #[idl("GPUResource")]
              enum Resource { Sampler(Sampler), Buffer(GpuBuffer), Binding(BufferBinding) }
              #[idl("GPUEntry")] struct Entry {}
              #[idl("GPUGroup")] struct Group {}
            "#,
            idl,
        )
        .unwrap();
        syn::parse_file(&generated).unwrap();
        assert!(generated.contains(
            "pub enum Resource { Sampler (i32) , Buffer (i32) , Binding (BufferBinding) }"
        ));
        assert!(generated.contains("pub (crate) resource : Option < Resource >"));
        assert!(generated.contains("fn new (i32) -> Box < Entry >"));
        assert!(generated.contains("fn resourceSampler (& mut Entry , & Sampler)"));
        assert!(generated.contains("fn resourceBinding (& mut Entry , & BufferBinding)"));
        assert!(generated.contains("this . resource = Some (Resource :: Buffer (value . handle))"));
        assert!(generated.contains("fn addExtrasBuffer (& mut Group , & GpuBuffer)"));
        assert!(
            !generated.contains("class Resource"),
            "a union is not a Caribou class"
        );

        let not_an_alternative = generate(
            "gpu",
            r#"
              trait Queue {}
              #[idl("GPUSampler")] trait Sampler {}
              #[idl("GPUResource")] enum Resource { Sampler(Sampler), Queue(Queue) }
            "#,
            idl,
        );
        assert!(not_an_alternative.is_err());
    }
    #[test]
    fn ambiguous_and_unsupported_declarations_fail_generation() {
        for (api, idl) in [
            ("#[idl(\"E\")] enum E {}", "enum E { \"a-b\", \"a--b\" };"),
            ("enum E {}", ""),
            ("trait R { fn call(this: &R); }", ""),
            ("trait R { #[native(call)] fn call(bytes: *mut u8); }", ""),
            (
                "trait R { #[native(call)] fn call(this: &R) -> Box<Missing>; }",
                "",
            ),
            (
                "#[idl(\"E\")] enum E {}",
                "enum E { \"a\" }; enum E { \"b\" };",
            ),
            ("struct R { new: Option<i32> }", ""),
        ] {
            assert!(generate("gpu", api, idl).is_err(), "accepted {api}");
        }
    }
}
