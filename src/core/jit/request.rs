use std::collections::HashMap;

use gqlforge_valid::Validator;
use gqlrs_value::ConstValue;
use serde::Deserialize;

use super::{Builder, OperationPlan, Result, Variables, transform};
use crate::core::Transform;
use crate::core::blueprint::Blueprint;
use crate::core::transform::TransformerOps;

#[derive(Debug, Deserialize, Clone)]
pub struct Request<V> {
    #[serde(default)]
    pub query: String,
    #[serde(default, rename = "operationName")]
    pub operation_name: Option<String>,
    #[serde(default)]
    pub variables: Variables<V>,
    #[serde(default)]
    pub extensions: HashMap<String, V>,
}

// NOTE: This is hot code and should allocate minimal memory
impl From<gqlrs::Request> for Request<ConstValue> {
    fn from(mut value: gqlrs::Request) -> Self {
        let variables = std::mem::take(&mut *value.variables);

        Self {
            query: value.query,
            operation_name: value.operation_name,
            variables: variables
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<Variables<_>>(),
            extensions: value.extensions.0,
        }
    }
}

impl Request<ConstValue> {
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn create_plan(&self, blueprint: &Blueprint) -> Result<OperationPlan<gqlrs_value::Value>> {
        let doc = gqlrs::parser::parse_query(&self.query)?;
        let builder = Builder::new(blueprint, &doc);
        let plan = builder.build(self.operation_name.as_deref())?;

        transform::CheckConst::new()
            .pipe(transform::CheckProtected::new())
            .pipe(transform::AuthPlanner::new())
            .pipe(transform::CheckDedupe::new())
            .pipe(transform::CheckCache::new())
            .pipe(transform::GraphQL::new())
            .transform(plan)
            .to_result()
            // both transformers are infallible right now
            // but we can't just unwrap this in stable rust
            // so convert to the Unknown error
            .map_err(|_| super::Error::Unknown)
    }
}

impl<V> Request<V> {
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
            operation_name: None,
            variables: Variables::new(),
            extensions: HashMap::new(),
        }
    }

    #[must_use]
    pub fn variables(self, vars: impl IntoIterator<Item = (String, V)>) -> Self {
        Self { variables: Variables::from_iter(vars), ..self }
    }
}
