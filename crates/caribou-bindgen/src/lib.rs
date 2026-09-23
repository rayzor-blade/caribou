//! Typed native binding declarations to Rust wrappers and `plugin!` exports.
//! Traits describe resource classes; structs describe plugin-owned records;
//! `#[native(name)]` selects a backend function. `#[idl("Name")]` imports
//! enum values or namespace constants from a vendored WebIDL source. This
//! does not infer native GPU semantics from WebIDL interfaces or generate a
//! language-specific heap layout.
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
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
fn ident(name: &str) -> Result<syn::Ident, String> {
    syn::parse_str(name).map_err(error)
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
        result.insert_str(0, "D");
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
        .windows(3)
        .enumerate()
        .filter(|(_, t)| t[0] == kind && t[1] == name && t[2] == "{")
        .collect();
    if matches.len() != 1 {
        return Err(format!(
            "expected one WebIDL {kind} {name}, found {}",
            matches.len()
        ));
    }
    let start = matches[0].0 + 3;
    let end = tokens[start..]
        .iter()
        .position(|s| s == "}")
        .ok_or_else(|| format!("unclosed {name}"))?;
    Ok(&tokens[start..start + end])
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
fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(p) = ty else { return None };
    p.path.get_ident().map(ToString::to_string)
}
fn scalar(ty: &Type) -> bool {
    type_name(ty).is_some_and(|s| {
        matches!(
            s.as_str(),
            "i32" | "u32" | "i64" | "f32" | "f64" | "bool" | "Text" | "Buffer"
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
        if type_name(ty).as_deref() == Some("Buffer") {
            return Err("Buffer fields need an explicit ownership policy".into());
        }
        if type_name(ty).as_deref() == Some("Text") {
            return Ok((quote!(String), quote!(Text), quote!(value.to_string())));
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
    let enums: HashSet<_> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Enum(e) => Some(e.ident.to_string()),
            _ => None,
        })
        .collect();
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
            Item::Enum(e) => {
                let name = &e.ident;
                let schema = format!("{namespace}.{name}");
                let variants: Vec<(syn::Ident, syn::Expr)> =
                    if let Some(source) = idl_name(&e.attrs)? {
                        if !e.variants.is_empty() {
                            return Err("IDL enums must have empty bodies".into());
                        }
                        enum_values(&idl, &source)?
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| Ok((ident(&pascal(&v))?, syn::parse_quote!(#i))))
                            .collect::<Result<_, String>>()?
                    } else {
                        let mut next: syn::Expr = syn::parse_quote!(0);
                        let mut values = Vec::new();
                        for v in &e.variants {
                            if !matches!(v.fields, syn::Fields::Unit) {
                                return Err("native enums must be fieldless".into());
                            }
                            let value = v
                                .discriminant
                                .as_ref()
                                .map(|(_, e)| e.clone())
                                .unwrap_or(next);
                            next = syn::parse_quote!((#value) + 1);
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
                let values: Vec<_> = variants.iter().map(|(_, e)| e).collect();
                let first = ids[0];
                output.extend(quote! {
                    #[derive(Debug, Clone, Copy, PartialEq, Eq, caribou_abi::PluginEnum)]
                    #[caribou(name = #schema)]
                    pub enum #name { #(#ids),* }
                    impl #name {
                        pub fn native(self) -> i32 { match self { #(Self::#ids => #values as i32),* } }
                        pub fn from_native(value: i32) -> Option<Self> {
                            #(if value == #values as i32 { return Some(Self::#ids); })* None
                        }
                    }
                    impl Default for #name { fn default() -> Self { Self::#first } }
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
                let syn::Fields::Named(fields) = s.fields else {
                    return Err("records need named fields".into());
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
                for field in fields.named {
                    if !field.attrs.is_empty() {
                        return Err(format!(
                            "record field attributes are not supported on {class}"
                        ));
                    }
                    let field_name = field.ident.expect("named field");
                    if !field_names.insert(field_name.to_string()) {
                        return Err(format!("duplicate field {class}.{field_name}"));
                    }
                    let (container, value_ty) = if let Some(inner) = generic(&field.ty, "Option") {
                        ("option", inner)
                    } else if let Some(inner) = generic(&field.ty, "Vec") {
                        ("sequence", inner)
                    } else {
                        ("required", field.ty.clone())
                    };
                    if let Some(enumeration) = generic(&value_ty, "Enum") {
                        if !enums.contains(&type_name(&enumeration).unwrap_or_default()) {
                            return Err(format!("unknown enum in {class}.{field_name}"));
                        }
                    }
                    let (stored, parameter, convert) =
                        stored_value(&value_ty, &resources, &records)?;
                    match container {
                        "required" => {
                            stored_fields.extend(quote!(pub(crate) #field_name: #stored,));
                            required_params.push(quote!(#field_name: #parameter));
                            required_types.push(quote!(#parameter));
                            required_values
                                .push(quote!(#field_name: { let value = #field_name; #convert }));
                        }
                        "option" => {
                            if !method_names.insert(field_name.to_string()) {
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
                            stored_fields.extend(quote!(pub(crate) #field_name: Vec<#stored>,));
                            initial_values.push(quote!(#field_name: Vec::new()));
                            let add = ident(&format!("add{}", pascal(&field_name.to_string())))?;
                            if !method_names.insert(add.to_string()) {
                                return Err(format!(
                                    "generated method {class}.{add} is duplicated"
                                ));
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
                            if i == 0 && target != class.to_string() {
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
                                    _ => quote!(Default::default()),
                                };
                                (quote!(value), fallback)
                            } else {
                                return Err("unsupported return type".into());
                            };
                            (quote!(-> #ty), convert, fallback)
                        }
                    };
                    methods.extend(quote! {
                        pub extern "C" fn #name(#(#params),*) #return_type {
                            let value = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { backend::#native(#(#args),*) })) {
                                Ok(value) => value,
                                Err(error) => {
                                    let message = error.downcast_ref::<String>().map(String::as_str)
                                        .or_else(|| error.downcast_ref::<&str>().copied()).unwrap_or("native backend panicked");
                                    caribou_abi::host::raise(caribou_abi::ErrorKind::Runtime, message);
                                    #fallback
                                }
                            };
                            #convert
                        }
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
    fn generated_code_contains_typed_objects_and_no_foreign_string_abi() {
        let generated = generate("gpu", r#"
          #[idl("Power")] enum Power {}
          #[idl("Usage")] mod Usage {}
          trait Device {
            #[native(create)] fn new() -> Box<Device>;
            #[native(shader)] fn shader(this: &Device, source: Text, data: Buffer, power: Enum<Power>) -> Box<Shader>;
          }
          trait Shader { #[native(name)] fn name(this: &Shader) -> Text; }
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
        assert!(generated.contains("pub (crate) label : Option < String >"));
        assert!(generated.contains("pub (crate) format : Option < i32 >"));
        assert!(generated.contains("pub (crate) buffer : Option < i32 >"));
        assert!(generated.contains("pub (crate) entries : Vec < Entry >"));
        assert!(generated.contains("fn new (size : i64) -> Box < Descriptor >"));
        assert!(generated.contains("fn label (& mut Descriptor , Text)"));
        assert!(generated.contains("fn addEntries (& mut Descriptor , & Entry)"));
        assert!(generated.contains("backend :: create (this . handle , descriptor)"));
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
            ("struct R { bytes: Buffer }", ""),
        ] {
            assert!(generate("gpu", api, idl).is_err(), "accepted {api}");
        }
    }
}
