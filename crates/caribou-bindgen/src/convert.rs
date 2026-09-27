//! A plugin's records as the wire's dictionaries, for a web backend: each
//! record imported from a WebIDL dictionary gets `wire()`, which builds the
//! dictionary the wire encodes from what the record holds. A member pairs
//! with its dictionary member by type: numbers and booleans as they are, a
//! text as a string, an imported enum by its IDL index (its native value),
//! a resource by its handle, a record through its own `wire()`, sequences,
//! nullables and unions item by item. A member the backend overrides, or
//! one that does not pair, is converted by the web module's function named
//! after the record and member (`gpu_compute_pipeline_descriptor_layout`)
//! when it defines one. A member the wire cannot carry makes the conversion
//! fail when it is set, naming it.

use std::collections::{BTreeSet, HashMap, HashSet};

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Type;

use crate::idl::{Model, Ty};
use crate::wire;
use crate::{generic, generic_pair, type_name};

/// Where a record's member came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The WebIDL dictionary, as it declares it.
    Imported,
    /// The WebIDL dictionary, with a type the plugin chose.
    Override,
    /// The plugin, beyond the dictionary.
    Extension,
}

/// A record as the plugin declares it.
pub(crate) struct Record {
    pub class: syn::Ident,
    /// The WebIDL dictionary it imports.
    pub source: Option<String>,
    pub fields: Vec<(syn::Ident, Type, Origin)>,
}

/// What the plugin declares that a conversion reads.
pub(crate) struct Plugin {
    pub records: Vec<Record>,
    /// Each union's variants, and whether a variant is an extension.
    pub unions: HashMap<String, Vec<(syn::Ident, Type, bool)>>,
    /// Each imported WebIDL name, and the plugin type it became.
    pub idl_types: HashMap<String, Type>,
    /// The plugin's resource classes.
    pub resources: HashSet<String>,
}

struct Ctx<'a> {
    plugin: &'a Plugin,
    model: &'a Model,
    /// The records whose dictionary the wire has, so `wire()` exists.
    converted: HashMap<String, String>,
    /// Each plugin type's WebIDL name.
    idl_of: HashMap<String, String>,
}

/// `wire()` for every record whose dictionary the wire has. `emitted` is
/// the wire's named types; `defined` the web module's functions.
pub(crate) fn conversions(
    plugin: &Plugin,
    model: &Model,
    emitted: &BTreeSet<String>,
    defined: &HashSet<String>,
) -> Result<TokenStream, String> {
    let converted = plugin
        .records
        .iter()
        .filter_map(|r| {
            let source = r.source.as_ref()?;
            emitted
                .contains(source)
                .then(|| (r.class.to_string(), source.clone()))
        })
        .collect();
    let idl_of = plugin
        .idl_types
        .iter()
        .map(|(idl, ty)| (quote!(#ty).to_string(), idl.clone()))
        .collect();
    let ctx = Ctx {
        plugin,
        model,
        converted,
        idl_of,
    };
    let mut out = TokenStream::new();
    for record in &plugin.records {
        let Some(source) = ctx.converted.get(&record.class.to_string()) else {
            continue;
        };
        out.extend(ctx.record(record, source, defined));
    }
    Ok(out)
}

impl Ctx<'_> {
    fn record(&self, record: &Record, source: &str, defined: &HashSet<String>) -> TokenStream {
        let class = &record.class;
        let dictionary = format_ident!("{source}");
        let members = self.model.dictionary_members(source);
        let carried: Vec<_> = members
            .iter()
            .filter(|m| wire::carried(self.model, &m.ty))
            .collect();
        let mut checks = TokenStream::new();
        let mut values = Vec::new();
        for m in &carried {
            let wire_field: syn::Ident =
                syn::parse_str(&wire::field(&m.name)).expect("a field name");
            let found = record
                .fields
                .iter()
                .find(|(name, _, origin)| *origin != Origin::Extension && unraw(name) == m.name);
            let unavailable = format!("`{class}.{}` is not available on the web", m.name);
            let value = match found {
                None if m.required => quote!(return Err(#unavailable.to_owned())),
                None => quote!(None),
                Some((name, ty, origin)) => {
                    let hook = format!(
                        "{}_{}",
                        wire::snake(&class.to_string()),
                        wire::snake(&m.name)
                    );
                    let generated = if *origin == Origin::Override {
                        None
                    } else {
                        self.member(
                            quote!(self.#name),
                            ty,
                            &m.ty,
                            m.required,
                            &format!("{class}.{}", m.name),
                        )
                    };
                    if defined.contains(&hook) {
                        let hook = format_ident!("{hook}");
                        quote!(crate::web::#hook(&self.#name)?)
                    } else if let Some(generated) = generated {
                        generated
                    } else if m.required {
                        quote!(return Err(#unavailable.to_owned()))
                    } else {
                        checks.extend(self.unset(quote!(self.#name), ty, &unavailable));
                        quote!(None)
                    }
                }
            };
            values.push(quote!(#wire_field: #value));
        }
        // What the wire has no member for must be unset.
        for (name, ty, origin) in &record.fields {
            let listed =
                *origin != Origin::Extension && carried.iter().any(|m| m.name == unraw(name));
            if !listed {
                let unavailable = format!("`{class}.{}` is not available on the web", unraw(name));
                checks.extend(self.unset(quote!(self.#name), ty, &unavailable));
            }
        }
        quote! {
            impl crate::#class {
                /// This record as the wire's dictionary, or which member the
                /// web has no way to carry.
                #[allow(clippy::all, unreachable_code)]
                pub(crate) fn wire(&self) -> Result<crate::wire::#dictionary, String> {
                    #checks
                    Ok(crate::wire::#dictionary { #(#values),* })
                }
            }
        }
    }

    /// A member held as `declared` (as the record declares it) as the
    /// dictionary member of type `ty`.
    fn member(
        &self,
        v: TokenStream,
        declared: &Type,
        ty: &Ty,
        required: bool,
        what: &str,
    ) -> Option<TokenStream> {
        let missing = format!("`{what}` is not set");
        // Held as an option: a nullable member, or an optional one.
        if let Some(inner) = generic(declared, "Option")
            && !matches!(ty, Ty::Nullable(_))
        {
            let x = self.value(quote!(x), &inner, ty, what)?;
            return Some(if required {
                quote!(match &#v { Some(x) => #x, None => return Err(#missing.to_owned()) })
            } else {
                quote!(match &#v { Some(x) => Some(#x), None => None })
            });
        }
        // A required union is held as an option, set through its setters.
        if type_name(declared).is_some_and(|n| self.plugin.unions.contains_key(&n)) {
            let x = self.value(quote!(x), declared, ty, what)?;
            return Some(if required {
                quote!(match &#v { Some(x) => #x, None => return Err(#missing.to_owned()) })
            } else {
                quote!(match &#v { Some(x) => Some(#x), None => None })
            });
        }
        let x = self.value(quote!((&#v)), declared, ty, what)?;
        Some(if required { x } else { quote!(Some(#x)) })
    }

    /// The expression turning `v`, a reference to a value held for
    /// `declared`, into the wire's value of `ty`; `None` when they do not
    /// pair.
    fn value(&self, v: TokenStream, declared: &Type, ty: &Ty, what: &str) -> Option<TokenStream> {
        self.paired(v, declared, ty, what, false)
    }

    /// As `value`; `alternative` when `ty` is one of a union's, which a
    /// resource pairs with only by its WebIDL name.
    fn paired(
        &self,
        v: TokenStream,
        declared: &Type,
        ty: &Ty,
        what: &str,
        alternative: bool,
    ) -> Option<TokenStream> {
        let name = type_name(declared);
        Some(match ty {
            Ty::Boolean if name.as_deref() == Some("bool") => quote!(*#v),
            Ty::Integer(i) if numeric(name.as_deref()) => {
                let int = format_ident!("{}", wire::int_name(*i));
                quote!(*#v as #int)
            }
            Ty::Float(double) if numeric(name.as_deref()) => {
                if *double {
                    quote!(*#v as f64)
                } else {
                    quote!(*#v as f32)
                }
            }
            Ty::String if name.as_deref() == Some("Text") => quote!(#v.get().as_str().to_owned()),
            Ty::Enum(e) => {
                let local = generic(declared, "Enum")?;
                let imported = self.idl_of.get(&quote!(Enum<#local>).to_string())?;
                if imported != e {
                    return None;
                }
                let e = format_ident!("{e}");
                let message = format!("`{what}`: this value is not available on the web");
                quote!(crate::wire::#e::from_index(*#v as u32).ok_or_else(|| #message.to_owned())?)
            }
            Ty::Interface(interface) => {
                let resource = name.filter(|n| self.plugin.resources.contains(n))?;
                match self.idl_of.get(&resource) {
                    Some(imported) if imported != interface => return None,
                    None if alternative => return None,
                    _ => {}
                }
                quote!(crate::wire::Handle(*#v as u32))
            }
            Ty::Dictionary(d) => {
                let record = name?;
                if self.converted.get(&record) != Some(d) {
                    return None;
                }
                quote!(#v.wire()?)
            }
            Ty::Sequence(t) => {
                let item = generic(declared, "Vec")?;
                let x = self.value(quote!(x), &item, t, what)?;
                quote! {
                    #v.iter().map(|x| -> Result<_, String> { Ok(#x) }).collect::<Result<Vec<_>, String>>()?
                }
            }
            Ty::Record(t) => {
                let (key, item) = generic_pair(declared, "Map")?;
                if type_name(&key).as_deref() != Some("Text") {
                    return None;
                }
                let x = self.value(quote!(x), &item, t, what)?;
                quote! {
                    #v.iter()
                        .map(|(k, x)| -> Result<_, String> { Ok((k.get().as_str().to_owned(), #x)) })
                        .collect::<Result<Vec<_>, String>>()?
                }
            }
            Ty::Nullable(t) => {
                let inner = generic(declared, "Option")?;
                let x = self.value(quote!(x), &inner, t, what)?;
                quote!(match #v { Some(x) => Some(#x), None => None })
            }
            Ty::Union(_, alternatives) => {
                let union = name?;
                let variants = self.plugin.unions.get(&union)?;
                let local = format_ident!("{union}");
                let wire_union = format_ident!("{}", wire::union_name(ty));
                let mut arms = Vec::new();
                for (variant, vty, _) in variants {
                    let paired = alternatives.iter().find_map(|a| {
                        let x = self.paired(quote!(x), vty, a, what, true)?;
                        let alternative = format_ident!("{}", wire::alternative_name(a));
                        Some(quote!(crate::#local::#variant(x) => crate::wire::#wire_union::#alternative(#x)))
                    });
                    arms.push(paired.unwrap_or_else(|| {
                        let message = format!("`{what}`: {variant} is not available on the web");
                        quote!(crate::#local::#variant(_) => return Err(#message.to_owned()))
                    }));
                }
                quote!(match #v { #(#arms),* })
            }
            _ => return None,
        })
    }
}

impl Ctx<'_> {
    /// The check that a member the wire cannot carry is unset.
    fn unset(&self, v: TokenStream, declared: &Type, message: &str) -> TokenStream {
        let union = type_name(declared).is_some_and(|n| self.plugin.unions.contains_key(&n));
        let set = if generic(declared, "Option").is_some() || union {
            quote!(#v.is_some())
        } else if generic(declared, "Vec").is_some() || generic_pair(declared, "Map").is_some() {
            quote!(!#v.is_empty())
        } else {
            quote!(true)
        };
        quote!(if #set { return Err(#message.to_owned()); })
    }
}

fn unraw(name: &syn::Ident) -> String {
    use syn::ext::IdentExt;
    name.unraw().to_string()
}

fn numeric(name: Option<&str>) -> bool {
    matches!(name, Some("i32" | "u32" | "i64" | "f32" | "f64"))
}
