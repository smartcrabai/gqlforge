use std::borrow::Cow;

use crate::core::jit::model::{Field, OperationPlan, Variables};
use crate::core::jit::store::{DataPath, Store};
use crate::core::jit::{Error, PathSegment, Positioned, ValidationError};
use crate::core::json::{JsonLike, JsonObjectLike};
use crate::core::scalar::Scalar;

type ValueStore<Value> = Store<Result<Value, Positioned<Error>>>;

pub struct Synth<'a, Value> {
    plan: &'a OperationPlan<Value>,
    store: ValueStore<Value>,
    variables: Variables<Value>,
}

impl<'a, Value> Synth<'a, Value> {
    #[inline]
    pub fn new(
        plan: &'a OperationPlan<Value>,
        store: ValueStore<Value>,
        variables: Variables<Value>,
    ) -> Self {
        Self { plan, store, variables }
    }
}

impl<'a, Value> Synth<'a, Value>
where
    Value: JsonLike<'a> + Clone + std::fmt::Debug,
{
    #[inline]
    fn include(&self, field: &Field<Value>) -> bool {
        !field.skip(&self.variables)
    }

    #[inline]
    pub fn synthesize<Output>(&'a self) -> Result<Output, Box<Positioned<Error>>>
    where
        Output: JsonLike<'a>,
    {
        let mut data = Output::JsonObject::with_capacity(self.plan.selection.len());
        let mut path = Vec::new();
        let root_name = self.plan.root_name();

        for child in &self.plan.selection {
            if !self.include(child) {
                continue;
            }
            // TODO: in case of error set `child.output_name` to null
            // and append error to response error array
            let val =
                self.process_node(child, None, &DataPath::new(), &mut path, Some(root_name))?;
            data.insert_key(&child.output_name, val);
        }

        Ok(Output::object(data))
    }

    #[inline]
    fn process_node<Output>(
        &'a self,
        node: &'a Field<Value>,
        value: Option<&'a Value>,
        data_path: &DataPath,
        path: &mut Vec<PathSegment<'a>>,
        root_name: Option<&'a str>,
    ) -> Result<Output, Box<Positioned<Error>>>
    where
        Output: JsonLike<'a>,
    {
        path.push(PathSegment::Field(Cow::Borrowed(&node.output_name)));

        let result = match self.store.get(node.id) {
            Some(value) => {
                let mut value = value.as_ref().map_err(|e| Box::new(e.clone()))?;

                for index in data_path.as_slice() {
                    if let Some(arr) = value.as_array() {
                        value = &arr[*index];
                    } else {
                        return Ok(Output::null());
                    }
                }

                // Only opaque JSON-like scalars are exempt from the shape
                // guard below -- a `JSON` field backed by a resolver that
                // happens to return an array (e.g. `LRANGE`) is not a shape
                // mismatch, it's just what the scalar holds, and
                // `iter_inner`'s scalar branch returns the value as-is
                // regardless of shape. All other scalars (built-in
                // String/Int/Boolean/Float/ID, which fall back to
                // `Scalar::Empty`, as well as validated custom scalars like
                // `Date`/`Email`) must still declare their shape correctly,
                // so keep the parity check for them.
                let is_opaque_scalar = matches!(node.scalar, Some(Scalar::JSON));
                if !is_opaque_scalar && node.type_of.is_list() != value.as_array().is_some() {
                    return Self::node_nullable_guard(node, path, None);
                }
                self.iter_inner(node, value, data_path, path)
            }
            None => match value {
                Some(result) => self.iter_inner(node, result, data_path, path),
                None => Self::node_nullable_guard(node, path, root_name),
            },
        };

        path.pop();
        result
    }

    /// This guard ensures to return Null value only if node type permits it, in
    /// case it does not it throws an Error
    fn node_nullable_guard<Output>(
        node: &'a Field<Value>,
        path: &[PathSegment],
        root_name: Option<&'a str>,
    ) -> Result<Output, Box<Positioned<Error>>>
    where
        Output: JsonLike<'a>,
    {
        if let Some(root_name) = root_name
            && node.name.eq("__typename")
        {
            return Ok(Output::string(Cow::Borrowed(root_name)));
        }
        // according to GraphQL spec https://spec.graphql.org/October2021/#sec-Handling-Field-Errors
        if node.type_of.is_nullable() {
            Ok(Output::null())
        } else {
            Err(ValidationError::ValueRequired.into())
                .map_err(|e| Self::to_location_error(e, node, path))
        }
    }

    #[inline]
    fn iter_inner<Output>(
        &'a self,
        node: &'a Field<Value>,
        value: &'a Value,
        data_path: &DataPath,
        path: &mut Vec<PathSegment<'a>>,
    ) -> Result<Output, Box<Positioned<Error>>>
    where
        Output: JsonLike<'a>,
    {
        // skip the field if field is not included in schema
        if !self.include(node) {
            return Ok(Output::null());
        }

        let eval_result = if value.is_null() {
            // check the nullability of this type unwrapping list modifier
            let is_nullable = match &node.type_of {
                crate::core::Type::Named { non_null, .. } => !*non_null,
                crate::core::Type::List { of_type, .. } => of_type.is_nullable(),
            };
            if is_nullable {
                Ok(Output::null())
            } else {
                Err(ValidationError::ValueRequired.into())
            }
        } else if let Some(scalar) = node.scalar.as_ref() {
            // TODO: add validation for input type as well. But input types are
            // not checked by gqlrs anyway so it should be done
            // after replacing default engine with JIT
            if scalar.validate(value) {
                Ok(Output::clone_from(value))
            } else {
                Err(ValidationError::ScalarInvalid { type_of: node.type_of.name().clone() }.into())
            }
        } else if node.is_enum {
            let check_valid_enum = |value: &Value| -> bool {
                value
                    .as_str()
                    .is_some_and(|v| self.plan.field_validate_enum_value(node, v))
            };

            let is_valid_enum = if let Some(vec) = value.as_array() {
                vec.iter().all(check_valid_enum)
            } else {
                check_valid_enum(value)
            };

            if is_valid_enum {
                Ok(Output::clone_from(value))
            } else {
                Err(ValidationError::EnumInvalid { type_of: node.type_of.name().clone() }.into())
            }
        } else {
            match (value.as_array(), value.as_object()) {
                (_, Some(obj)) => {
                    let mut fields = Vec::with_capacity(node.selection.len());

                    for child in node
                        .iter()
                        .filter(|field| self.plan.field_is_part_of_value(field, value))
                    {
                        // all checks for skip must occur in `iter_inner`
                        // and include be checked before calling `iter` or
                        // recursing.
                        if self.include(child) {
                            let value = if child.name == "__typename" {
                                Output::string(node.value_type(value).into())
                            } else {
                                let val = obj.get_key(child.name.as_str());
                                self.process_node(child, val, data_path, path, None)?
                            };
                            fields.push((child.output_name.as_str(), value));
                        }
                    }

                    Ok(Output::object(Output::JsonObject::from_vec(fields)))
                }
                (Some(arr), _) => {
                    let mut ans = Vec::with_capacity(arr.len());
                    for (i, val) in arr.iter().enumerate() {
                        path.push(PathSegment::Index(i));
                        let val =
                            self.iter_inner(node, val, &data_path.clone().with_index(i), path)?;
                        path.pop();
                        ans.push(val);
                    }
                    Ok(Output::array(ans))
                }
                _ => Ok(Output::clone_from(value)),
            }
        };

        eval_result.map_err(|e| Self::to_location_error(e, node, path))
    }

    fn to_location_error(
        error: Error,
        node: &'a Field<Value>,
        path: &[PathSegment],
    ) -> Box<Positioned<Error>> {
        Box::new(
            Positioned::new(error, node.pos).with_path(
                path.iter()
                    .map(|x| match x {
                        PathSegment::Field(cow) => {
                            PathSegment::Field(Cow::Owned(cow.clone().into_owned()))
                        }
                        PathSegment::Index(i) => PathSegment::Index(*i),
                    })
                    .collect(),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use gqlforge_valid::Validator;
    use gqlrs_value::ConstValue;
    use serde::{Deserialize, Serialize};

    use super::ValueStore;
    use crate::core::blueprint::Blueprint;
    use crate::core::config::{Config, ConfigModule};
    use crate::core::jit::OperationPlan;
    use crate::core::jit::builder::Builder;
    use crate::core::jit::fixtures::JP;
    use crate::core::jit::model::{FieldId, Variables};
    use crate::core::jit::store::Store;
    use crate::core::jit::synth::Synth;
    use crate::core::json::JsonLike;

    const POSTS: &str = r#"
        [
                {
                    "id": 1,
                    "userId": 1,
                    "title": "Some Title"
                },
                {
                    "id": 2,
                    "userId": 1,
                    "title": "Not Some Title"
                }
        ]
    "#;

    const USER1: &str = r#"
        {
                "id": 1,
                "name": "foo"
        }
    "#;

    const USER2: &str = r#"
        {
                "id": 2,
                "name": "bar"
        }
    "#;
    const USERS: &str = r#"
        [
          {
            "id": 1,
            "name": "Leanne Graham"
          },
          {
            "id": 2,
            "name": "Ervin Howell"
          }
        ]
    "#;

    #[derive(Clone)]
    enum TestData {
        Posts,
        UsersData,
        Users,
        User1,
    }

    impl TestData {
        fn into_value<'a, Value: Deserialize<'a> + JsonLike<'a>>(self) -> Value {
            match self {
                Self::Posts => serde_json::from_str(POSTS).unwrap(),
                Self::User1 => serde_json::from_str(USER1).unwrap(),
                TestData::UsersData => Value::array(vec![
                    serde_json::from_str(USER1).unwrap(),
                    serde_json::from_str(USER2).unwrap(),
                ]),
                TestData::Users => serde_json::from_str(USERS).unwrap(),
            }
        }
    }

    const CONFIG: &str = include_str!("../fixtures/jsonplaceholder-mutation.graphql");

    fn make_store<'a, Value>(
        query: &str,
        store: Vec<(FieldId, TestData)>,
    ) -> (OperationPlan<Value>, ValueStore<Value>, Variables<Value>)
    where
        Value: Deserialize<'a> + JsonLike<'a> + Serialize + Clone + std::fmt::Debug,
    {
        let store = store
            .into_iter()
            .map(|(id, data)| (id, data.into_value()))
            .collect::<Vec<_>>();

        let doc = gqlrs::parser::parse_query(query).unwrap();
        let config = Config::from_sdl(CONFIG).to_result().unwrap();
        let config = ConfigModule::from(config);

        let builder = Builder::new(&Blueprint::try_from(&config).unwrap(), &doc);
        let plan = builder.build(None).unwrap();
        let plan = plan
            .try_map(|v| {
                // Earlier we hard OperationPlan<ConstValue> which has impl
                // Deserialize but now InputResolver takes
                // OperationPlan<gqlrs_value::Value> and returns
                // OperationPlan<gqlrs_value::Value>. So we need
                // to map Plan to some other value before being able to
                // deserialize it.
                let serde = v.into_json().unwrap();
                Deserialize::deserialize(serde)
            })
            .unwrap();

        let store = store
            .into_iter()
            .fold(Store::new(), |mut store, (id, data)| {
                store.set_data(id, Ok(data));
                store
            });
        let vars = Variables::new();

        (plan, store, vars)
    }

    fn assert_synths(query: &str, store: &[(FieldId, TestData)]) {
        let (plan, value_store, vars) = make_store::<ConstValue>(query, store.to_vec());
        let synth_const = Synth::new(&plan, value_store, vars);
        let (plan, value_store, vars) =
            make_store::<serde_json_borrow::Value>(query, store.to_vec());
        let synth_borrow = Synth::new(&plan, value_store, vars);

        let val_const: ConstValue = synth_const.synthesize().unwrap();
        let val_const = serde_json::to_string_pretty(&val_const).unwrap();
        let val_borrow: serde_json_borrow::Value = synth_borrow.synthesize().unwrap();
        let val_borrow = serde_json::to_string_pretty(&val_borrow).unwrap();
        assert_eq!(val_const, val_borrow);
    }

    #[test]
    fn test_posts() {
        let store = vec![(FieldId::new(0), TestData::Posts)];
        let query = r"
            query {
                posts { id }
            }
        ";

        assert_synths(query, &store);
    }

    #[test]
    fn test_user() {
        let store = vec![(FieldId::new(0), TestData::User1)];
        let query = r"
                query {
                    user(id: 1) { id }
                }
            ";

        assert_synths(query, &store);
    }

    #[test]
    fn test_nested() {
        let store = vec![
            (FieldId::new(0), TestData::Posts),
            (FieldId::new(3), TestData::UsersData),
        ];
        let query = r"
                query {
                    posts { id title user { id name } }
                }
            ";
        assert_synths(query, &store);
    }

    #[test]
    fn test_multiple_nested() {
        let store = vec![
            (FieldId::new(0), TestData::Posts),
            (FieldId::new(3), TestData::UsersData),
            (FieldId::new(6), TestData::Users),
        ];
        let query = r"
                query {
                    posts { id title user { id name } }
                    users { id name }
                }
            ";
        assert_synths(query, &store);
    }

    #[test]
    fn test_json_placeholder() {
        let jp: JP<gqlrs::Value> = JP::init("{ posts { id title userId user { id name } } }", None);
        let synth = jp.synth();
        let val: gqlrs::Value = synth.synthesize().unwrap();
        insta::assert_snapshot!(serde_json::to_string_pretty(&val).unwrap());
    }

    #[test]
    fn test_json_placeholder_borrowed() {
        let jp: JP<serde_json_borrow::Value> =
            JP::init("{ posts { id title userId user { id name } } }", None);
        let synth = jp.synth();
        let val: serde_json_borrow::Value = synth.synthesize().unwrap();
        insta::assert_snapshot!(serde_json::to_string_pretty(&val).unwrap());
    }

    #[test]
    fn test_json_placeholder_typename() {
        let jp: JP<serde_json_borrow::Value> =
            JP::init("{ posts { id __typename user { __typename id } } }", None);
        let synth = jp.synth();
        let val: serde_json_borrow::Value = synth.synthesize().unwrap();
        insta::assert_snapshot!(serde_json::to_string_pretty(&val).unwrap());
    }

    #[test]
    fn test_json_placeholder_typename_root_level() {
        let jp: JP<serde_json_borrow::Value> =
            JP::init("{ __typename posts { id user { id }} }", None);
        let synth = jp.synth();
        let val: serde_json_borrow::Value = synth.synthesize().unwrap();
        insta::assert_snapshot!(serde_json::to_string_pretty(&val).unwrap());
    }

    /// Regression test for a JIT-layer bug (independent of any particular
    /// data source): a field declared as a scalar (here `JSON`, a `Named`,
    /// non-list type) whose resolver returns an array value must not be
    /// nulled out by the list/array shape-parity guard in `process_node`.
    /// Scalars are opaque leaves -- `LRANGE` (Redis), or any resolver
    /// returning a JSON array for a `JSON`-typed field, is exactly this
    /// shape and must round-trip unchanged.
    #[test]
    fn test_scalar_field_with_array_value_is_not_nulled() {
        const SCHEMA: &str = r#"
            schema {
                query: Query
            }
            type Query {
                jobs: JSON @expr(body: "placeholder")
            }
        "#;

        let config = Config::from_sdl(SCHEMA).to_result().unwrap();
        let config = ConfigModule::from(config);
        let blueprint = Blueprint::try_from(&config).unwrap();

        let doc = gqlrs::parser::parse_query("{ jobs }").unwrap();
        let builder = Builder::new(&blueprint, &doc);
        let plan = builder.build(None).unwrap();
        let plan = plan
            .try_map(|v| {
                let serde = v.into_json().unwrap();
                Deserialize::deserialize(serde)
            })
            .unwrap();

        let mut store: super::ValueStore<ConstValue> = Store::new();
        store.set_data(
            FieldId::new(0),
            Ok(ConstValue::List(vec![
                ConstValue::String("job-1".to_string()),
                ConstValue::String("job-2".to_string()),
            ])),
        );

        let synth = Synth::new(&plan, store, Variables::new());
        let val: ConstValue = synth.synthesize().unwrap();

        assert_eq!(
            val,
            ConstValue::Object(
                [(
                    gqlrs::Name::new("jobs"),
                    ConstValue::List(vec![
                        ConstValue::String("job-1".to_string()),
                        ConstValue::String("job-2".to_string())
                    ])
                )]
                .into()
            )
        );
    }

    /// Regression test: unlike opaque scalars such as `JSON`, an ordinary
    /// non-list scalar field (here `String`, which falls back to
    /// `Scalar::Empty`, see `builder.rs`) must still respect the
    /// list/array shape-parity guard. A resolver that returns an array for
    /// a non-list `String` field is a contract violation and must be
    /// nulled out rather than passed through as-is.
    #[test]
    fn test_non_list_scalar_field_with_array_value_is_nulled() {
        const SCHEMA: &str = r#"
            schema {
                query: Query
            }
            type Query {
                name: String @expr(body: "placeholder")
            }
        "#;

        let config = Config::from_sdl(SCHEMA).to_result().unwrap();
        let config = ConfigModule::from(config);
        let blueprint = Blueprint::try_from(&config).unwrap();

        let doc = gqlrs::parser::parse_query("{ name }").unwrap();
        let builder = Builder::new(&blueprint, &doc);
        let plan = builder.build(None).unwrap();
        let plan = plan
            .try_map(|v| {
                let serde = v.into_json().unwrap();
                Deserialize::deserialize(serde)
            })
            .unwrap();

        let mut store: super::ValueStore<ConstValue> = Store::new();
        store.set_data(
            FieldId::new(0),
            Ok(ConstValue::List(vec![
                ConstValue::String("unexpected-1".to_string()),
                ConstValue::String("unexpected-2".to_string()),
            ])),
        );

        let synth = Synth::new(&plan, store, Variables::new());
        let val: ConstValue = synth.synthesize().unwrap();

        assert_eq!(
            val,
            ConstValue::Object([(gqlrs::Name::new("name"), ConstValue::Null)].into())
        );
    }

    /// Regression test: a list scalar field (here `[String]`, which also
    /// falls back to `Scalar::Empty`) must still respect the
    /// list/array shape-parity guard. A resolver that returns a bare
    /// scalar for a `[String]` field is a contract violation and must be
    /// nulled out rather than passed through as-is.
    #[test]
    fn test_list_scalar_field_with_bare_scalar_value_is_nulled() {
        const SCHEMA: &str = r#"
            schema {
                query: Query
            }
            type Query {
                tags: [String] @expr(body: ["placeholder"])
            }
        "#;

        let config = Config::from_sdl(SCHEMA).to_result().unwrap();
        let config = ConfigModule::from(config);
        let blueprint = Blueprint::try_from(&config).unwrap();

        let doc = gqlrs::parser::parse_query("{ tags }").unwrap();
        let builder = Builder::new(&blueprint, &doc);
        let plan = builder.build(None).unwrap();
        let plan = plan
            .try_map(|v| {
                let serde = v.into_json().unwrap();
                Deserialize::deserialize(serde)
            })
            .unwrap();

        let mut store: super::ValueStore<ConstValue> = Store::new();
        store.set_data(
            FieldId::new(0),
            Ok(ConstValue::String("bare-tag".to_string())),
        );

        let synth = Synth::new(&plan, store, Variables::new());
        let val: ConstValue = synth.synthesize().unwrap();

        assert_eq!(
            val,
            ConstValue::Object([(gqlrs::Name::new("tags"), ConstValue::Null)].into())
        );
    }
}
