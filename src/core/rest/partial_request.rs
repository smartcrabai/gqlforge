use gqlrs::parser::types::ExecutableDocument;
use gqlrs::{Name, Variables};
use gqlrs_value::ConstValue;

use super::path::Path;
use super::{Request, Result};
use crate::core::gqlrs_hyper::GraphQLRequest;

/// A partial `GraphQLRequest` that contains a parsed executable GraphQL
/// document.
#[derive(Debug)]
pub struct PartialRequest<'a> {
    pub body: Option<&'a String>,
    pub doc: &'a ExecutableDocument,
    pub variables: Variables,
    pub path: &'a Path,
}

impl PartialRequest<'_> {
    pub async fn into_request(self, request: Request) -> Result<GraphQLRequest> {
        let mut variables = self.variables;
        if let Some(key) = self.body {
            let bytes = http_body_util::BodyExt::collect(request.into_body())
                .await
                .unwrap()
                .to_bytes();
            let body: ConstValue = serde_json::from_slice(&bytes)?;
            variables.insert(Name::new(key), body);
        }

        let mut req = gqlrs::Request::new("").variables(variables);
        req.set_parsed_query(self.doc.clone());

        Ok(GraphQLRequest(req))
    }
}
