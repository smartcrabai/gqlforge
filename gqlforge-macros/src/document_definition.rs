extern crate proc_macro;
use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

#[derive(Default)]
struct DirectiveDefinitionAttr {
    is_repeatable: bool,
    is_lowercase_name: bool,
    locations: Option<String>,
}

fn get_directive_definition_attr(input: &DeriveInput) -> syn::Result<DirectiveDefinitionAttr> {
    let mut directive_definition_attr = DirectiveDefinitionAttr::default();
    for attr in &input.attrs {
        if attr.path().is_ident("directive_definition") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("repeatable") {
                    directive_definition_attr.is_repeatable = true;
                }

                if meta.path.is_ident("locations") {
                    let value = meta.value()?;
                    let s: syn::LitStr = value.parse()?;
                    directive_definition_attr.locations = Some(s.value());
                }

                if meta.path.is_ident("lowercase_name") {
                    directive_definition_attr.is_lowercase_name = true;
                }

                Ok(())
            })?;
        }
    }

    Ok(directive_definition_attr)
}

pub fn expand_directive_definition(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_identifier = &input.ident;

    let directive_definition_attr = match get_directive_definition_attr(&input) {
        Ok(attr) => attr,
        Err(err) => panic!("{err}"),
    };
    let is_repeatable = directive_definition_attr.is_repeatable;
    let is_lowercase_name = directive_definition_attr.is_lowercase_name;
    let locations = if let Some(locations) = directive_definition_attr.locations {
        locations
            .split(',')
            .map(|location| location.trim().to_string())
            .collect::<Vec<String>>()
    } else {
        vec![]
    };

    let expanded = quote! {
        impl gqlforge_typedefs_common::directive_definition::DirectiveDefinition for #struct_identifier {
            fn directive_definition(generated_types: &mut std::collections::HashSet<String>) -> Vec<gqlrs::parser::types::TypeSystemDefinition> {
                let schemars = gqlforge_typedefs_common::into_schemars::<Self>();
                let attr = gqlforge_typedefs_common::directive_definition::Attrs {
                    name: stringify!(#struct_identifier),
                    repeatable: #is_repeatable,
                    locations: vec![#(#locations),*],
                    is_lowercase_name: #is_lowercase_name
                };
                gqlforge_typedefs_common::directive_definition::into_directive_definition(&schemars, attr, generated_types)
            }
        }
    };

    TokenStream::from(expanded)
}

pub fn expand_input_definition(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_identifier = &input.ident;

    let expanded = quote! {
        impl gqlforge_typedefs_common::input_definition::InputDefinition for #struct_identifier {
            fn input_definition() -> gqlrs::parser::types::TypeSystemDefinition {
                let schemars = gqlforge_typedefs_common::into_schemars::<Self>();
                gqlforge_typedefs_common::input_definition::into_input_definition_from_schema(&schemars, stringify!(#struct_identifier))
            }
        }
    };

    TokenStream::from(expanded)
}
