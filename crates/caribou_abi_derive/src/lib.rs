//! Derive the static schema and direct heap conversion of ordinary Rust enums.
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, Fields, LitStr, Pat, Path, parse_macro_input, spanned::Spanned,
};

#[proc_macro_derive(PluginEnum, attributes(caribou))]
pub fn plugin_enum(input: TokenStream) -> TokenStream {
    expand(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "plugin enums cannot be generic",
        ));
    }
    let mut name: Option<LitStr> = None;
    let mut source: Option<Path> = None;
    let mut fallback: Option<Expr> = None;
    for attr in &input.attrs {
        if attr.path().is_ident("caribou") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    name = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("from") {
                    source = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("fallback") {
                    fallback = Some(meta.value()?.parse()?);
                } else {
                    return Err(meta.error("expected name, from or fallback"));
                }
                Ok(())
            })?;
        }
    }
    let name = name.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "add #[caribou(name = \"plugin.EnumName\")]",
        )
    })?;
    if fallback.is_some() && source.is_none() {
        return Err(syn::Error::new(
            input.ident.span(),
            "fallback requires from",
        ));
    }
    let Data::Enum(data) = input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "PluginEnum can only be derived for enums",
        ));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "plugin enums need at least one variant",
        ));
    }
    let ident = input.ident;
    let mut descriptors = Vec::new();
    let mut encode = Vec::new();
    let mut decode = Vec::new();
    let mut convert = Vec::new();
    let mut visit = Vec::new();
    for (index, variant) in data.variants.iter().enumerate() {
        let index = index as u32;
        let v = &variant.ident;
        let mut variant_name = LitStr::new(&v.to_string(), v.span());
        let mut pattern: Option<Pat> = None;
        let mut skip = false;
        for attr in &variant.attrs {
            if attr.path().is_ident("caribou") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("name") {
                        variant_name = meta.value()?.parse()?;
                    } else if meta.path.is_ident("pattern") {
                        pattern = Some(meta.value()?.call(Pat::parse_single)?);
                    } else if meta.path.is_ident("skip") {
                        skip = true;
                    } else {
                        return Err(meta.error("expected name, pattern or skip"));
                    }
                    Ok(())
                })?;
            }
        }
        if (skip || pattern.is_some()) && source.is_none() {
            return Err(syn::Error::new(v.span(), "pattern and skip require from"));
        }
        if skip && pattern.is_some() {
            return Err(syn::Error::new(
                v.span(),
                "a skipped variant cannot have a source pattern",
            ));
        }
        let mut binds = Vec::new();
        let mut field_descs = Vec::new();
        let mut to_values = Vec::new();
        let mut from_values = Vec::new();
        let mut converted = Vec::new();
        for (i, field) in variant.fields.iter().enumerate() {
            let binding = field.ident.clone().unwrap_or_else(|| format_ident!("a{i}"));
            let mut field_name = LitStr::new(&binding.to_string(), binding.span());
            let mut value: Option<Expr> = None;
            for attr in &field.attrs {
                if attr.path().is_ident("caribou") {
                    attr.parse_nested_meta(|meta| {
                        if meta.path.is_ident("name") {
                            field_name = meta.value()?.parse()?;
                        } else if meta.path.is_ident("value") {
                            value = Some(meta.value()?.parse()?);
                        } else {
                            return Err(meta.error("expected name or value"));
                        }
                        Ok(())
                    })?;
                }
            }
            if value.is_some() && (source.is_none() || skip) {
                return Err(syn::Error::new(
                    field.span(),
                    "value requires a mapped source variant",
                ));
            }
            let ty = &field.ty;
            field_descs.push(quote! {
                ::caribou_abi::data::EnumFieldDesc {
                    name: ::caribou_abi::Str::new(#field_name),
                    tag: <#ty as ::caribou_abi::EnumField>::TAG,
                    enumeration: <#ty as ::caribou_abi::EnumField>::ENUM,
                }
            });
            to_values.push(quote!(::caribou_abi::Kept::new(<#ty as ::caribou_abi::EnumField>::into_value(#binding))));
            from_values.push(
                quote!(<#ty as ::caribou_abi::EnumField>::from_value(__caribou_value.fields()[#i])),
            );
            converted.push(match value {
                Some(value) => quote!(#value),
                None => quote!(::core::convert::Into::into(#binding)),
            });
            binds.push(binding);
        }
        let count = binds.len();
        let construct = |fields: &[proc_macro2::TokenStream]| match &variant.fields {
            Fields::Unit => quote!(Self::#v),
            Fields::Unnamed(_) => quote!(Self::#v(#(#fields),*)),
            Fields::Named(_) => quote!(Self::#v { #(#binds: #fields),* }),
        };
        let own_pattern = match &variant.fields {
            Fields::Unit => quote!(Self::#v),
            Fields::Unnamed(_) => quote!(Self::#v(#(#binds),*)),
            Fields::Named(_) => quote!(Self::#v { #(#binds),* }),
        };
        let decoded = construct(&from_values);
        let converted = construct(&converted);
        descriptors.push(quote! {
            ::caribou_abi::data::VariantDesc {
                name: ::caribou_abi::Str::new(#variant_name),
                fields: &[#(#field_descs),*] as *const _,
                field_count: #count,
            }
        });
        encode.push(quote! {
            #own_pattern => {
                // Root a completed field before another can allocate.
                let __caribou_roots: [::caribou_abi::Kept; #count] = [#(#to_values),*];
                let __caribou_values = __caribou_roots.each_ref().map(|r| r.get());
                ::caribou_abi::Enum::new(#index, &__caribou_values)
            }
        });
        decode.push(quote!(#index => #decoded));
        visit.push(quote! {
            #own_pattern => { #(::caribou_abi::EnumField::visit(#binds, __caribou_visit);)* }
        });
        if let Some(source) = &source
            && !skip
        {
            let source_pattern = pattern.map_or_else(
                || match &variant.fields {
                    Fields::Unit => quote!(#source::#v),
                    Fields::Unnamed(_) => quote!(#source::#v(#(#binds),*)),
                    Fields::Named(_) => quote!(#source::#v { #(#binds),* }),
                },
                |p| quote!(#p),
            );
            convert.push(quote!(#source_pattern => #converted));
        }
    }
    let count = descriptors.len();
    let conversion = source.map(|source| {
        let fallback = fallback.map(|fallback| quote!(_ => #fallback,));
        quote! {
            impl ::core::convert::From<#source> for #ident {
                fn from(__caribou_source: #source) -> Self {
                    match __caribou_source { #(#convert,)* #fallback }
                }
            }
        }
    });
    Ok(quote! {
        impl ::caribou_abi::PluginEnum for #ident {
            const DESC: &'static ::caribou_abi::EnumDesc = &::caribou_abi::EnumDesc {
                name: ::caribou_abi::Str::new(#name),
                variants: &[#(#descriptors),*] as *const _,
                variant_count: #count,
            };
            fn encode(self) -> ::caribou_abi::Enum<Self> {
                let __caribou_inputs = ::caribou_abi::data::EnumRoots::new(&self);
                match self { #(#encode),* }
            }
            fn decode(__caribou_value: ::caribou_abi::Enum<Self>) -> Self {
                match __caribou_value.index() {
                    #(#decode,)*
                    _ => unreachable!("host validated enum tag"),
                }
            }
        }
        impl ::caribou_abi::EnumField for #ident {
            const TAG: ::caribou_abi::TypeTag = ::caribou_abi::TypeTag::ENUM;
            const ENUM: *const ::caribou_abi::EnumDesc = <Self as ::caribou_abi::PluginEnum>::DESC;
            fn into_value(self) -> ::caribou_abi::Value {
                <Self as ::caribou_abi::PluginEnum>::encode(self).value()
            }
            fn from_value(value: ::caribou_abi::Value) -> Self {
                ::caribou_abi::Enum::<Self>::of(value).expect("host validated enum field").get()
            }
            fn visit(&self, __caribou_visit: &mut dyn FnMut(::caribou_abi::Value)) {
                match self { #(#visit),* }
            }
        }
        #conversion
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_declarations() {
        for (input, message) in [
            (
                quote!(
                    struct Bad;
                ),
                "add #[caribou",
            ),
            (
                quote!(
                    #[caribou(name = "test.Bad")]
                    struct Bad;
                ),
                "only be derived for enums",
            ),
            (
                quote!(
                    #[caribou(name = "test.Bad")]
                    enum Bad<T> {
                        Value(T),
                    }
                ),
                "cannot be generic",
            ),
            (
                quote!(
                    #[caribou(name = "test.Bad")]
                    enum Bad {}
                ),
                "at least one variant",
            ),
            (
                quote!(
                    #[caribou(name = "test.Bad")]
                    enum Bad {
                        #[caribou(skip)]
                        A,
                    }
                ),
                "require from",
            ),
            (
                quote!(
                    #[caribou(name = "test.Bad")]
                    enum Bad {
                        A(#[caribou(value = 1)] i32),
                    }
                ),
                "requires a mapped source",
            ),
        ] {
            let error = expand(syn::parse2(input).unwrap()).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}

/// One `plugin!` member, exported under its link symbol
/// (`caribou_mangle::symbol`): the name an AOT call reaches it by. The
/// input is what `plugin!` gathered for the member: the plugin's name, the
/// class (`""` for a free function), the path to the item, its name, its
/// parameter types and its result type. The kind and arity are read from
/// the signature as the plugin loader reads them: `new` returning the
/// class is its constructor, a first parameter `&Class` or `&mut Class`
/// its receiver, anything else a static; a free function is a static of
/// the class named after the plugin.
#[proc_macro]
pub fn plugin_link_export(input: TokenStream) -> TokenStream {
    link_export(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct LinkMember {
    plugin: LitStr,
    class: Option<syn::Ident>,
    path: Path,
    method: syn::Ident,
    params: Vec<syn::Type>,
    ret: Option<syn::Type>,
}

impl syn::parse::Parse for LinkMember {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let plugin: LitStr = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let class = if input.peek(LitStr) {
            input.parse::<LitStr>()?;
            None
        } else {
            Some(input.parse()?)
        };
        input.parse::<syn::Token![,]>()?;
        let path: Path = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let method: syn::Ident = input.parse()?;
        input.parse::<syn::Token![,]>()?;
        let params;
        syn::parenthesized!(params in input);
        let params = params
            .parse_terminated(syn::Type::parse, syn::Token![,])?
            .into_iter()
            .collect();
        input.parse::<syn::Token![,]>()?;
        let ret;
        syn::bracketed!(ret in input);
        let ret = if ret.is_empty() {
            None
        } else {
            Some(ret.parse()?)
        };
        Ok(LinkMember {
            plugin,
            class,
            path,
            method,
            params,
            ret,
        })
    }
}

/// `ty` without the invisible group a `macro_rules` `$ty:ty` fragment
/// arrives wrapped in, or parentheses.
fn bare(mut ty: &syn::Type) -> &syn::Type {
    loop {
        match ty {
            syn::Type::Group(g) => ty = &g.elem,
            syn::Type::Paren(p) => ty = &p.elem,
            _ => return ty,
        }
    }
}

/// Whether `ty` is `&class` or `&mut class`.
fn is_receiver(ty: &syn::Type, class: &syn::Ident) -> bool {
    match bare(ty) {
        syn::Type::Reference(r) => matches!(bare(&r.elem), syn::Type::Path(p) if p.path.is_ident(class)),
        _ => false,
    }
}

/// Whether `ty` is `Box<class>`.
fn boxes(ty: &syn::Type, class: &syn::Ident) -> bool {
    let syn::Type::Path(p) = bare(ty) else { return false };
    let Some(last) = p.path.segments.last() else { return false };
    if last.ident != "Box" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else { return false };
    matches!(args.args.first(), Some(syn::GenericArgument::Type(inner)) if matches!(bare(inner), syn::Type::Path(inner) if inner.path.is_ident(class)))
}

fn link_export(input: proc_macro2::TokenStream) -> syn::Result<proc_macro2::TokenStream> {
    let m: LinkMember = syn::parse2(input)?;
    let plugin = m.plugin.value();
    let method = m.method.to_string();
    let method = method.strip_prefix("r#").unwrap_or(&method);
    let (class, kind, arity) = match &m.class {
        None => (capitalised(&plugin), 't', m.params.len()),
        Some(class) => {
            let constructs = method == "new" && m.ret.as_ref().is_some_and(|r| boxes(r, class));
            let receives = m.params.first().is_some_and(|t| is_receiver(t, class));
            let (kind, arity) = if constructs {
                ('c', m.params.len())
            } else if receives {
                ('m', m.params.len() - 1)
            } else {
                ('t', m.params.len())
            };
            (class.to_string(), kind, arity)
        }
    };
    let symbol = caribou_mangle::symbol(&plugin, &class, &class, kind, method, arity);
    let path = &m.path;
    let args: Vec<proc_macro2::Ident> = (0..m.params.len()).map(|i| format_ident!("a{i}")).collect();
    let params = &m.params;
    let ret = m.ret.as_ref().map(|r| quote!(-> #r));
    Ok(quote! {
        const _: () = {
            #[unsafe(export_name = #symbol)]
            pub extern "C" fn __caribou_linked(#(#args: #params),*) #ret {
                #path(#(#args),*)
            }
        };
    })
}

/// A plugin's name as the class its free functions hang in.
fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
