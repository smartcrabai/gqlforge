use gqlrs::parser::types::{
    EnumType, EnumValueDefinition, TypeDefinition, TypeKind, TypeSystemDefinition,
};
use gqlrs::{Name, Positioned};
use schemars::Schema;
use serde_json::Value;

#[derive(Debug)]
pub struct EnumVariant {
    pub value: String,
    pub description: Option<Positioned<String>>,
}

impl EnumVariant {
    pub fn new(value: String) -> Self {
        Self { value, description: None }
    }
}

#[derive(Debug)]
pub struct EnumValue {
    pub variants: Vec<EnumVariant>,
    pub description: Option<Positioned<String>>,
}

use crate::common::{get_description, pos};

pub fn into_enum_definition(enum_value: EnumValue, name: &str) -> TypeSystemDefinition {
    let mut enum_value_definition = vec![];
    for enum_variant in enum_value.variants {
        let formatted_value: String = enum_variant
            .value
            .clone()
            .chars()
            .filter(|ch| ch != &'"')
            .collect();
        enum_value_definition.push(pos(EnumValueDefinition {
            value: pos(Name::new(formatted_value)),
            description: enum_variant.description,
            directives: vec![],
        }));
    }

    TypeSystemDefinition::Type(pos(TypeDefinition {
        name: pos(Name::new(name)),
        kind: TypeKind::Enum(EnumType { values: enum_value_definition }),
        description: enum_value.description,
        directives: vec![],
        extend: false,
    }))
}

pub fn into_enum_value(schema: &Schema) -> Option<EnumValue> {
    let obj = schema.as_object()?;
    let description = get_description(obj).map(|d| pos(d.to_owned()));

    // Simple enum: { "enum": ["Var1", "Var2", ...] }
    if let Some(Value::Array(enum_values)) = obj.get("enum") {
        let variants = enum_values
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| EnumVariant::new(s.to_string()))
            .collect::<Vec<_>>();
        if !variants.is_empty() {
            return Some(EnumValue { variants, description });
        }
    }

    // Single const value (used in oneOf for enums with docs in schemars 1.x)
    if let Some(const_val) = obj.get("const")
        && let Some(s) = const_val.as_str()
    {
        return Some(EnumValue { variants: vec![EnumVariant::new(s.to_string())], description });
    }

    // Enum with per-variant docs: { "oneOf": [{ "const": "V1", ... }, ...] }
    if let Some(Value::Array(one_ofs)) = obj.get("oneOf") {
        let variants = one_ofs
            .iter()
            .filter_map(|one_of| {
                let one_of_schema: &Schema = (one_of).try_into().ok()?;
                // try to parse one_of value as enum
                into_enum_value(one_of_schema).and_then(|mut en| {
                    // if it has only single variant it's our high-level enum
                    if en.variants.len() == 1 {
                        let variant = en.variants.pop()?;
                        Some(EnumVariant { value: variant.value, description: en.description })
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();

        if !variants.is_empty() {
            return Some(EnumValue { variants, description });
        }
    }

    None
}
