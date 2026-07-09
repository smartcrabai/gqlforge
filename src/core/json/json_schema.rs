use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use convert_case::{Case, Casing};
use gqlforge_valid::{Valid, Validator};
use prost_reflect::{EnumDescriptor, FieldDescriptor, Kind, MessageDescriptor};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename = "schema")]
pub enum JsonSchema {
    Obj(BTreeMap<String, JsonSchema>),
    Arr(Box<JsonSchema>),
    Opt(Box<JsonSchema>),
    Enum(BTreeSet<String>),
    Str,
    Num,
    Bool,
    Empty,
    Any,
}

impl Display for JsonSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonSchema::Obj(fields) => {
                let mut fields = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<String>>();

                fields.sort();

                write!(f, "{{{}}}", fields.join(", "))
            }
            JsonSchema::Arr(schema) => {
                write!(f, "[{schema}]")
            }
            JsonSchema::Opt(schema) => {
                write!(f, "Option<{schema}>")
            }
            JsonSchema::Enum(en) => {
                let mut en = en.iter().cloned().collect::<Vec<String>>();
                en.sort();
                write!(f, "enum {{{}}}", en.join(", "))
            }
            JsonSchema::Str => {
                write!(f, "String")
            }
            JsonSchema::Num => {
                write!(f, "Number")
            }
            JsonSchema::Bool => {
                write!(f, "Boolean")
            }
            JsonSchema::Empty => {
                write!(f, "Empty")
            }
            JsonSchema::Any => {
                write!(f, "Any")
            }
        }
    }
}

impl<const L: usize> From<[(&'static str, JsonSchema); L]> for JsonSchema {
    fn from(fields: [(&'static str, JsonSchema); L]) -> Self {
        let mut map = BTreeMap::new();
        for (name, schema) in fields {
            map.insert(name.to_string(), schema);
        }
        JsonSchema::Obj(map)
    }
}

impl Default for JsonSchema {
    fn default() -> Self {
        JsonSchema::Obj(BTreeMap::new())
    }
}

impl JsonSchema {
    #[must_use]
    pub fn from_scalar_type(type_name: &str) -> Self {
        match type_name {
            "String" => JsonSchema::Str,
            "Int" => JsonSchema::Num,
            "Boolean" => JsonSchema::Bool,
            "Empty" => JsonSchema::Empty,
            _ => JsonSchema::Any,
        }
    }

    // TODO: validate `JsonLike` instead of fixing on `gqlrs::Value`
    pub fn validate(&self, value: &gqlrs::Value) -> Valid<(), &'static str> {
        match self {
            JsonSchema::Str => match value {
                gqlrs::Value::String(_) => Valid::succeed(()),
                _ => Valid::fail("expected string"),
            },
            JsonSchema::Num => match value {
                gqlrs::Value::Number(_) => Valid::succeed(()),
                _ => Valid::fail("expected number"),
            },
            JsonSchema::Bool => match value {
                gqlrs::Value::Boolean(_) => Valid::succeed(()),
                _ => Valid::fail("expected boolean"),
            },
            JsonSchema::Empty => match value {
                gqlrs::Value::Null => Valid::succeed(()),
                gqlrs::Value::Object(obj) if obj.is_empty() => Valid::succeed(()),
                _ => Valid::fail("expected empty"),
            },
            JsonSchema::Arr(schema) => match value {
                gqlrs::Value::List(list) => {
                    // TODO: add unit tests
                    Valid::from_iter(list.iter().enumerate(), |(i, item)| {
                        schema.validate(item).trace(i.to_string().as_str())
                    })
                    .unit()
                }
                _ => Valid::fail("expected array"),
            },
            JsonSchema::Obj(fields) => {
                let field_schema_list: Vec<(&String, &JsonSchema)> = fields.iter().collect();
                match value {
                    gqlrs::Value::Object(map) => {
                        Valid::from_iter(field_schema_list, |(name, schema)| {
                            if schema.is_required() {
                                if let Some(field_value) = map.get::<str>(name.as_ref()) {
                                    schema.validate(field_value).trace(name)
                                } else {
                                    Valid::fail("expected field to be non-nullable").trace(name)
                                }
                            } else if let Some(field_value) = map.get::<str>(name.as_ref()) {
                                schema.validate(field_value).trace(name)
                            } else {
                                Valid::succeed(())
                            }
                        })
                        .unit()
                    }
                    _ => Valid::fail("expected object"),
                }
            }
            JsonSchema::Opt(schema) => match value {
                gqlrs::Value::Null => Valid::succeed(()),
                _ => schema.validate(value),
            },
            JsonSchema::Any | JsonSchema::Enum(_) => Valid::succeed(()),
        }
    }

    /// Check if `self` is a subtype of `other`
    #[must_use]
    pub fn is_a(&self, super_type: &JsonSchema, name: &str) -> Valid<(), String> {
        let sub_type = self;
        if let JsonSchema::Any = super_type {
            return Valid::succeed(());
        }

        let fail = Valid::fail(format!(
            "Type '{sub_type}' is not assignable to type '{super_type}'"
        ))
        .trace(name);

        match super_type {
            JsonSchema::Str | JsonSchema::Num | JsonSchema::Bool | JsonSchema::Empty => {
                if super_type != sub_type {
                    return fail;
                }
            }
            JsonSchema::Any => {}
            JsonSchema::Obj(expected) => {
                if let JsonSchema::Obj(actual) = sub_type {
                    return Valid::from_iter(expected.iter(), |(key, expected)| {
                        Valid::from_option(actual.get(key), format!("missing key: {key}"))
                            .and_then(|actual| actual.is_a(expected, key))
                    })
                    .trace(name)
                    .unit();
                }
                return fail;
            }
            JsonSchema::Arr(expected) => {
                if let JsonSchema::Arr(actual) = sub_type {
                    return actual.is_a(expected, name);
                }
                return fail;
            }
            JsonSchema::Opt(expected) => {
                if let JsonSchema::Opt(actual) = sub_type {
                    return actual.is_a(expected, name);
                }
                return sub_type.is_a(expected, name);
            }
            JsonSchema::Enum(expected) => {
                if let JsonSchema::Enum(actual) = sub_type {
                    if actual.ne(expected) {
                        return fail;
                    }
                } else {
                    return fail;
                }
            }
        }
        Valid::succeed(())
    }

    #[must_use]
    pub fn optional(self) -> JsonSchema {
        JsonSchema::Opt(Box::new(self))
    }

    #[must_use]
    pub fn is_optional(&self) -> bool {
        matches!(self, JsonSchema::Opt(_))
    }

    #[must_use]
    pub fn is_required(&self) -> bool {
        !self.is_optional()
    }
}

impl TryFrom<&MessageDescriptor> for JsonSchema {
    type Error = gqlforge_valid::ValidationError<String>;

    fn try_from(value: &MessageDescriptor) -> Result<Self, Self::Error> {
        if value.is_map_entry() {
            // we encode protobuf's map as JSON scalar
            return Ok(JsonSchema::Any);
        }

        let mut map = BTreeMap::new();
        let fields = value.fields();

        for field in fields {
            let field_schema = JsonSchema::try_from(&field)?;

            // the snake_case for field names is automatically converted to camelCase
            // by prost on serde serialize/deserealize and in graphql type name should be in
            // camelCase as well, so convert field.name to camelCase here
            map.insert(field.name().to_case(Case::Camel), field_schema);
        }

        if map.is_empty() {
            Ok(JsonSchema::Empty)
        } else {
            Ok(JsonSchema::Obj(map))
        }
    }
}

impl TryFrom<&EnumDescriptor> for JsonSchema {
    type Error = gqlforge_valid::ValidationError<String>;

    fn try_from(value: &EnumDescriptor) -> Result<Self, Self::Error> {
        let mut set = BTreeSet::new();
        for value in value.values() {
            set.insert(value.name().to_string());
        }
        Ok(JsonSchema::Enum(set))
    }
}

impl TryFrom<&FieldDescriptor> for JsonSchema {
    type Error = gqlforge_valid::ValidationError<String>;

    fn try_from(value: &FieldDescriptor) -> Result<Self, Self::Error> {
        let field_schema = match value.kind() {
            Kind::Double
            | Kind::Float
            | Kind::Int32
            | Kind::Int64
            | Kind::Uint32
            | Kind::Uint64
            | Kind::Sint32
            | Kind::Sint64
            | Kind::Fixed32
            | Kind::Fixed64
            | Kind::Sfixed32
            | Kind::Sfixed64 => JsonSchema::Num,
            Kind::Bool => JsonSchema::Bool,
            Kind::String | Kind::Bytes => JsonSchema::Str,
            Kind::Message(msg) => JsonSchema::try_from(&msg)?,
            Kind::Enum(enm) => JsonSchema::try_from(&enm)?,
        };
        let field_schema = if value.supports_presence()
            && value.cardinality() != prost_reflect::Cardinality::Required
        {
            JsonSchema::Opt(Box::new(field_schema))
        } else {
            field_schema
        };
        let field_schema = if value.is_list() {
            JsonSchema::Arr(Box::new(field_schema))
        } else {
            field_schema
        };

        Ok(field_schema)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::collections::{BTreeMap, BTreeSet};

    use gqlforge_fixtures::protobuf;
    use gqlforge_valid::{Valid, Validator};
    use gqlrs::Name;
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    use crate::core::blueprint::GrpcMethod;
    use crate::core::grpc::protobuf::ProtobufSet;
    use crate::core::grpc::protobuf::tests::get_proto_file;
    use crate::core::json::JsonSchema;

    #[test]
    fn test_validate_string() {
        let schema = JsonSchema::Str;
        let value = gqlrs::Value::String("hello".to_string());
        let result = schema.validate(&value);
        assert_eq!(result, Valid::succeed(()));
    }

    #[test]
    fn test_validate_valid_object() {
        let schema = JsonSchema::from([("name", JsonSchema::Str), ("age", JsonSchema::Num)]);
        let value = gqlrs::Value::Object({
            let mut map = IndexMap::new();
            map.insert(Name::new("name"), gqlrs::Value::String("hello".to_string()));
            map.insert(Name::new("age"), gqlrs::Value::Number(1.into()));
            map
        });
        let result = schema.validate(&value);
        assert_eq!(result, Valid::succeed(()));
    }

    #[test]
    fn test_validate_invalid_object() {
        let schema = JsonSchema::from([("name", JsonSchema::Str), ("age", JsonSchema::Num)]);
        let value = gqlrs::Value::Object({
            let mut map = IndexMap::new();
            map.insert(Name::new("name"), gqlrs::Value::String("hello".to_string()));
            map.insert(Name::new("age"), gqlrs::Value::String("1".to_string()));
            map
        });
        let result = schema.validate(&value);
        assert_eq!(result, Valid::fail("expected number").trace("age"));
    }

    #[test]
    fn test_null_key() {
        let schema = JsonSchema::from([
            ("name", JsonSchema::Str.optional()),
            ("age", JsonSchema::Num),
        ]);
        let value = gqlrs::Value::Object({
            let mut map = IndexMap::new();
            map.insert(Name::new("age"), gqlrs::Value::Number(1.into()));
            map
        });

        let result = schema.validate(&value);
        assert_eq!(result, Valid::succeed(()));
    }

    #[test]
    fn test_empty_valid() {
        let schema = JsonSchema::from([
            ("empty1", JsonSchema::Empty.optional()),
            ("empty2", JsonSchema::Empty),
        ]);
        let value = gqlrs::Value::Object({
            let mut map = IndexMap::new();
            map.insert(Name::new("empty1"), gqlrs::Value::Object(IndexMap::new()));
            map.insert(Name::new("empty2"), gqlrs::Value::Null);
            map
        });

        let result = schema.validate(&value);
        assert_eq!(result, Valid::succeed(()));
    }

    #[test]
    fn test_empty_invalid() {
        let schema = JsonSchema::Empty;
        let value = gqlrs::Value::String("test".to_owned());

        let result = schema.validate(&value);
        assert_eq!(result, Valid::fail("expected empty"));
    }

    #[test]
    fn test_any_valid() {
        let schema = JsonSchema::from([
            ("any1", JsonSchema::Any.optional()),
            ("any2", JsonSchema::Any),
        ]);
        let value = gqlrs::Value::Object({
            let mut map = IndexMap::new();
            map.insert(Name::new("any1"), gqlrs::Value::Object(IndexMap::new()));
            map.insert(Name::new("any2"), gqlrs::Value::String("test".to_owned()));
            map
        });

        let result = schema.validate(&value);
        assert_eq!(result, Valid::succeed(()));
    }

    #[tokio::test]
    async fn test_from_protobuf_conversion() -> anyhow::Result<()> {
        let grpc_method = GrpcMethod::try_from("news.NewsService.GetNews").unwrap();

        let file = ProtobufSet::from_proto_file(get_proto_file(protobuf::NEWS).await?)?;
        let service = file.find_service(&grpc_method)?;
        let operation = service.find_operation(&grpc_method)?;

        let schema = JsonSchema::try_from(&operation.output_type)?;

        assert_eq!(
            schema,
            JsonSchema::Obj(BTreeMap::from_iter([
                ("postImage".to_owned(), JsonSchema::Str),
                ("title".to_owned(), JsonSchema::Str),
                ("id".to_owned(), JsonSchema::Num),
                ("body".to_owned(), JsonSchema::Str),
                (
                    "status".to_owned(),
                    JsonSchema::Enum(BTreeSet::from_iter([
                        "DELETED".to_owned(),
                        "DRAFT".to_owned(),
                        "PUBLISHED".to_owned()
                    ]))
                )
            ]))
        );

        Ok(())
    }
    #[test]
    fn test_compare_enum() {
        let mut en = BTreeSet::new();
        en.insert("A".to_string());
        en.insert("B".to_string());
        let value = JsonSchema::Arr(Box::new(JsonSchema::Enum(en.clone())));
        let schema = JsonSchema::Enum(en);
        let name = "foo";
        let result = schema.is_a(&value, name);
        assert_eq!(
            result,
            Valid::fail("Type 'enum {A, B}' is not assignable to type '[enum {A, B}]'".to_string())
                .trace(name)
        );
    }

    #[test]
    fn test_compare_enum_value() {
        let mut en = BTreeSet::new();
        en.insert("A".to_string());
        en.insert("B".to_string());

        let mut en1 = BTreeSet::new();
        en1.insert("A".to_string());
        en1.insert("B".to_string());
        en1.insert("C".to_string());

        let value = JsonSchema::Enum(en1.clone());
        let schema = JsonSchema::Enum(en.clone());
        let name = "foo";
        let result = schema.is_a(&value, name);
        assert_eq!(
            result,
            Valid::fail(
                "Type 'enum {A, B}' is not assignable to type 'enum {A, B, C}'".to_string()
            )
            .trace(name)
        );
    }

    #[test]
    fn test_covariance_optional() {
        let parent = JsonSchema::Str.optional();
        let child = JsonSchema::Str;
        let child_is_a_parent = child.is_a(&parent, "foo").is_succeed();
        assert!(child_is_a_parent);
    }

    #[test]
    fn test_covariance_object() {
        // type in Proto file
        let base = JsonSchema::from([
            ("id", JsonSchema::Num.optional()),
            ("title", JsonSchema::Str.optional()),
            ("body", JsonSchema::Str.optional()),
        ]);

        // type in GraphQL file
        let child = JsonSchema::from([
            ("id", JsonSchema::Num),
            ("title", JsonSchema::Str),
            ("body", JsonSchema::Str),
        ]);

        assert!(child.is_a(&base, "foo").is_succeed());
    }

    #[test]
    fn test_covariance_array() {
        // type in Proto file
        let base = JsonSchema::Arr(Box::new(JsonSchema::from([
            ("id", JsonSchema::Num.optional()),
            ("title", JsonSchema::Str.optional()),
            ("body", JsonSchema::Str.optional()),
        ])));

        // type in GraphQL file
        let child = JsonSchema::Arr(Box::new(JsonSchema::from([
            ("id", JsonSchema::Num),
            ("title", JsonSchema::Str),
            ("body", JsonSchema::Str),
        ])));

        assert!(child.is_a(&base, "foo").is_succeed());
    }
}
