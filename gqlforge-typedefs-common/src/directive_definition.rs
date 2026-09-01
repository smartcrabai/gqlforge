use std::collections::HashSet;
use std::hash::BuildHasher;

use gqlrs::Name;
use gqlrs::parser::types::{DirectiveLocation, TypeSystemDefinition};
use schemars::Schema;
use serde_json::Value;

use crate::common::{first_char_to_lower, first_char_to_upper, get_description, pos};
use crate::enum_definition::{into_enum_definition, into_enum_value};
use crate::input_definition::{into_input_definition, into_input_value_definition};

pub trait DirectiveDefinition {
    fn directive_definition(generated_types: &mut HashSet<String>) -> Vec<TypeSystemDefinition>;
}

#[derive(Clone)]
pub struct Attrs {
    pub name: &'static str,
    pub repeatable: bool,
    pub locations: Vec<&'static str>,
    pub is_lowercase_name: bool,
}

#[must_use]
pub fn from_directive_location(str: DirectiveLocation) -> String {
    match str {
        DirectiveLocation::Schema => String::from("SCHEMA"),
        DirectiveLocation::Object => String::from("OBJECT"),
        DirectiveLocation::EnumValue => String::from("ENUM_VALUE"),
        _ => String::from("FIELD_DEFINITION"),
    }
}

fn into_directive_location(str: &str) -> DirectiveLocation {
    match str {
        "Schema" => DirectiveLocation::Schema,
        "Object" => DirectiveLocation::Object,
        "EnumValue" => DirectiveLocation::EnumValue,
        _ => DirectiveLocation::FieldDefinition,
    }
}

pub fn into_directive_definition<S: BuildHasher>(
    root_schema: &Schema,
    attrs: Attrs,
    generated_types: &mut HashSet<String, S>,
) -> Vec<TypeSystemDefinition> {
    let Some(root_obj) = root_schema.as_object() else {
        return vec![];
    };

    let mut service_doc_definitions = vec![];
    let description = get_description(root_obj);

    // Get sub-type definitions from $defs (2020-12) or definitions (draft07)
    let definitions = root_obj
        .get("$defs")
        .or_else(|| root_obj.get("definitions"))
        .and_then(Value::as_object);

    if let Some(definitions) = definitions {
        for (name, schema_value) in definitions {
            if generated_types.contains(name) {
                continue;
            }
            // the definition could either be an enum or a type
            // we don't know which one is it, so we first try to get an
            // EnumValue if into_enum_value returns Some we can be
            // sure it's an Enum
            if let Ok(schema) = Schema::try_from(schema_value.clone()) {
                if let Some(enum_values) = into_enum_value(&schema) {
                    service_doc_definitions.push(into_enum_definition(enum_values, name));
                    generated_types.insert(name.clone());
                } else if let Some(obj) = schema.as_object() {
                    generated_types.insert(name.clone());
                    let mut capitalized_name = name.clone();
                    first_char_to_upper(&mut capitalized_name);
                    service_doc_definitions.push(into_input_definition(obj, &capitalized_name));
                }
            }
        }
    }

    let name = if attrs.is_lowercase_name {
        attrs.name.to_lowercase()
    } else {
        first_char_to_lower(attrs.name)
    };

    let directive_definition =
        TypeSystemDefinition::Directive(pos(gqlrs::parser::types::DirectiveDefinition {
            description: description.map(|inner| pos(inner.to_owned())),
            name: pos(Name::new(name)),
            arguments: into_input_value_definition(root_obj),
            is_repeatable: attrs.repeatable,
            locations: attrs
                .locations
                .into_iter()
                .map(|val| pos(into_directive_location(val)))
                .collect(),
        }));
    service_doc_definitions.push(directive_definition);
    service_doc_definitions
}
