use gqlrs::parser::types::{
    BaseType, InputObjectType, InputValueDefinition, Type, TypeDefinition, TypeKind,
    TypeSystemDefinition,
};
use gqlrs::{Name, Positioned};
use schemars::Schema;
use serde_json::{Map, Value};

use crate::common::{first_char_to_upper, get_description, pos};

pub trait InputDefinition {
    fn input_definition() -> TypeSystemDefinition;
}

#[must_use]
pub fn into_input_definition_from_schema(schema: &Schema, name: &str) -> TypeSystemDefinition {
    let empty = Map::new();
    let obj = schema.as_object().unwrap_or(&empty);
    into_input_definition(obj, name)
}

pub fn into_input_definition(schema: &Map<String, Value>, name: &str) -> TypeSystemDefinition {
    let description = get_description(schema).map(std::borrow::ToOwned::to_owned);

    TypeSystemDefinition::Type(pos(TypeDefinition {
        name: pos(Name::new(name)),
        kind: TypeKind::InputObject(InputObjectType {
            fields: into_input_value_definition(schema),
        }),
        description: description.map(pos),
        directives: vec![],
        extend: false,
    }))
}

#[must_use]
pub fn into_input_value_definition(
    schema: &Map<String, Value>,
) -> Vec<Positioned<InputValueDefinition>> {
    let mut arguments_type = vec![];

    let list = schema
        .get("anyOf")
        .or_else(|| schema.get("allOf"))
        .or_else(|| schema.get("oneOf"));

    if let Some(Value::Array(schemas)) = list {
        for sub_schema in schemas {
            if let Some(obj) = sub_schema.as_object() {
                arguments_type.extend(build_arguments_type(obj));
            }
        }
        return arguments_type;
    }

    build_arguments_type(schema)
}

fn build_arguments_type(schema: &Map<String, Value>) -> Vec<Positioned<InputValueDefinition>> {
    let mut arguments = vec![];

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return arguments;
    };

    let required_arr = schema.get("required").and_then(Value::as_array);
    let is_required = |name: &str| -> bool {
        required_arr.is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(name)))
    };

    for (name, property) in properties {
        let (property_obj, nullable_from_anyof) = if let Some(obj) = property.as_object() {
            unwrap_nullable(obj)
        } else {
            continue;
        };

        let Some(property_obj) = property_obj else {
            continue;
        };

        let description = get_description(property_obj);
        let nullable = !is_required(name) || nullable_from_anyof;
        let definition = pos(InputValueDefinition {
            description: description.map(|inner| pos(inner.to_owned())),
            name: pos(Name::new(name)),
            ty: pos(determine_input_value_type_from_schema(
                name.clone(),
                property_obj,
                nullable,
            )),
            default_value: None,
            directives: Vec::new(),
        });

        arguments.push(definition);
    }

    arguments
}

/// If schema is `{ "anyOf": [T, {"type":"null"}] }`, returns (Some(T), true).
/// Otherwise returns (Some(schema), false).
fn unwrap_nullable(schema: &Map<String, Value>) -> (Option<&Map<String, Value>>, bool) {
    if let Some(Value::Array(schemas)) = schema.get("anyOf") {
        let non_null: Vec<_> = schemas
            .iter()
            .filter(|s| {
                s.as_object()
                    .is_none_or(|o| o.get("type").and_then(Value::as_str) != Some("null"))
            })
            .collect();
        let has_null = schemas.len() != non_null.len();

        if has_null && non_null.len() == 1 {
            return (non_null[0].as_object(), true);
        }
    }
    (Some(schema), false)
}

fn determine_input_value_type_from_schema(
    mut name: String,
    schema: &Map<String, Value>,
    nullable: bool,
) -> Type {
    first_char_to_upper(&mut name);

    if let Some(type_value) = schema.get("type") {
        match type_value {
            Value::String(typ) => match typ.as_str() {
                "boolean" | "number" | "string" | "integer" => {
                    return Type {
                        nullable,
                        base: BaseType::Named(Name::new(get_type_name(typ))),
                    };
                }
                _ => {}
            },
            Value::Array(types) => {
                let non_null_types: Vec<&str> = types
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|&t| t != "null")
                    .collect();
                let is_nullable = types.iter().any(|t| t.as_str() == Some("null"));

                if let Some(&typ) = non_null_types.first() {
                    match typ {
                        "boolean" | "number" | "string" | "integer" => {
                            return Type {
                                nullable: nullable || is_nullable,
                                base: BaseType::Named(Name::new(get_type_name(typ))),
                            };
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    determine_type_from_schema(name, schema)
}

fn determine_type_from_schema(name: String, schema: &Map<String, Value>) -> Type {
    // Array type
    if let Some(items) = schema.get("items").or_else(|| schema.get("prefixItems")) {
        return determine_type_from_array(name, items);
    }

    // Object type with properties
    if let Some(Value::Object(props)) = schema.get("properties")
        && !props.is_empty()
    {
        return Type { nullable: true, base: BaseType::Named(Name::new(name)) };
    }

    // anyOf/allOf/oneOf - look for a $ref in the schemas
    let list = schema
        .get("anyOf")
        .or_else(|| schema.get("allOf"))
        .or_else(|| schema.get("oneOf"));

    if let Some(Value::Array(schemas)) = list {
        for s in schemas {
            if let Some(obj) = s.as_object()
                && let Some(Value::String(reference)) = obj.get("$ref")
            {
                return determine_type_from_reference(reference);
            }
        }
    }

    // Direct $ref
    if let Some(Value::String(reference)) = schema.get("$ref") {
        return determine_type_from_reference(reference);
    }

    Type { nullable: true, base: BaseType::Named(Name::new("JSON")) }
}

fn determine_type_from_reference(reference: &str) -> Type {
    let mut name = reference
        .split('/')
        .next_back()
        .unwrap_or(reference)
        .to_string();
    first_char_to_upper(&mut name);
    Type { nullable: true, base: BaseType::Named(Name::new(name)) }
}

fn determine_type_from_array(name: String, items: &Value) -> Type {
    match items {
        Value::Object(schema) => Type {
            nullable: true,
            base: BaseType::List(Box::new(determine_input_value_type_from_schema(
                name, schema, false,
            ))),
        },
        Value::Array(schemas) => {
            if let Some(Value::Object(schema)) = schemas.first() {
                Type {
                    nullable: true,
                    base: BaseType::List(Box::new(determine_input_value_type_from_schema(
                        name, schema, false,
                    ))),
                }
            } else {
                Type { nullable: true, base: BaseType::Named(Name::new("JSON")) }
            }
        }
        _ => Type { nullable: true, base: BaseType::Named(Name::new("JSON")) },
    }
}

fn get_type_name(typ: &str) -> String {
    match typ {
        "integer" => "Int".to_string(),
        "boolean" => "Boolean".to_string(),
        "number" => "Float".to_string(),
        "string" => "String".to_string(),
        other => {
            let mut s = other.to_string();
            first_char_to_upper(&mut s);
            s
        }
    }
}
