use std::sync::Arc;

use gqlforge_valid::{Cause, Valid, Validator};
use gqlrs::dynamic::Schema;

use super::Error;
use crate::core::blueprint::{Blueprint, SchemaModifiers};
use crate::core::gqlrs_hyper::{GraphQLRequest, GraphQLRequestLike};
use crate::core::http::RequestContext;

#[derive(Debug)]
pub struct OperationQuery {
    query: GraphQLRequest,
}

impl OperationQuery {
    #[must_use]
    pub fn new(query: GraphQLRequest, request_context: Arc<RequestContext>) -> Self {
        let query = query.data(request_context);
        Self { query }
    }

    async fn validate(self, schema: &Schema) -> Vec<Error> {
        schema
            .execute(self.query.0)
            .await
            .errors
            .iter()
            .map(|v| Error::GraphQLServer(v.clone()))
            .collect()
    }
}

pub async fn validate_operations(
    blueprint: &Blueprint,
    operations: Vec<OperationQuery>,
) -> Valid<(), String> {
    let schema = blueprint.to_schema_with(&SchemaModifiers::default().with_no_resolver());
    Valid::from_iter(
        futures_util::future::join_all(operations.into_iter().map(|op| op.validate(&schema)))
            .await
            .iter(),
        |errors| {
            if errors.is_empty() {
                Valid::succeed(())
            } else {
                Valid::<(), String>::from_vec_cause(
                    errors.iter().map(|e| Cause::new(e.to_string())).collect(),
                )
            }
        },
    )
    .unit()
}
