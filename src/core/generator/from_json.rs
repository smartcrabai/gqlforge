use std::collections::{BTreeMap, BTreeSet, HashMap};

use convert_case::{Case, Casing};
use gqlforge_valid::{Valid, Validator};
use serde_json::Value;
use url::Url;

use super::NameGenerator;
use super::json::{self, GraphQLTypesGenerator};
use crate::core::config::transformer::RenameTypes;
use crate::core::config::{Config, GraphQLOperationType};
use crate::core::http::Method;
use crate::core::merge_right::MergeRight;
use crate::core::transform::{Transform, TransformerOps};

pub struct RequestSample {
    pub url: Url,
    pub method: Method,
    pub req_body: Value,
    pub res_body: Value,
    pub field_name: String,
    pub operation_type: GraphQLOperationType,
    pub headers: Option<BTreeMap<String, String>>,
}

impl RequestSample {
    #[must_use]
    pub fn new(url: Url, response_body: Value, field_name: String) -> Self {
        Self {
            url,
            field_name,
            res_body: response_body,
            method: Method::default(),
            req_body: Value::default(),
            headers: None,
            operation_type: GraphQLOperationType::default(),
        }
    }

    #[must_use]
    pub fn with_method(mut self, method: Method) -> Self {
        self.method = method;
        self
    }

    #[must_use]
    pub fn with_req_body(mut self, req_body: Value) -> Self {
        self.req_body = req_body;
        self
    }

    #[must_use]
    pub fn with_headers(mut self, headers: Option<BTreeMap<String, String>>) -> Self {
        self.headers = headers;
        self
    }

    #[must_use]
    pub fn with_is_mutation(mut self, is_mutation: bool) -> Self {
        let operation_type = if is_mutation {
            GraphQLOperationType::Mutation
        } else {
            GraphQLOperationType::Query
        };
        self.operation_type = operation_type;
        self
    }

    #[must_use]
    pub fn with_is_subscription(mut self, is_subscription: bool) -> Self {
        if is_subscription {
            self.operation_type = GraphQLOperationType::Subscription;
        }
        self
    }
}

pub struct FromJsonGenerator<'a> {
    request_samples: &'a [RequestSample],
    type_name_generator: &'a NameGenerator,
    query_name: &'a str,
    mutation_name: &'a Option<String>,
    subscription_name: &'a Option<String>,
}

impl<'a> FromJsonGenerator<'a> {
    pub fn new(
        request_samples: &'a [RequestSample],
        type_name_generator: &'a NameGenerator,
        query_name: &'a str,
        mutation_name: &'a Option<String>,
        subscription_name: &'a Option<String>,
    ) -> Self {
        Self {
            request_samples,
            type_name_generator,
            query_name,
            mutation_name,
            subscription_name,
        }
    }
}

impl Transform for FromJsonGenerator<'_> {
    type Value = Config;
    type Error = String;
    fn transform(&self, config: Self::Value) -> Valid<Self::Value, Self::Error> {
        let config_gen_req = self.request_samples;
        let type_name_gen = self.type_name_generator;

        Valid::from_iter(config_gen_req, |sample| {
            let (existing_name, suggested_name) = match sample.operation_type {
                GraphQLOperationType::Query => (
                    GraphQLOperationType::Query
                        .to_string()
                        .to_case(Case::Pascal),
                    self.query_name.to_owned(),
                ),
                GraphQLOperationType::Mutation => (
                    GraphQLOperationType::Mutation
                        .to_string()
                        .to_case(Case::Pascal),
                    self.mutation_name.clone().unwrap_or("Mutation".to_owned()),
                ),
                GraphQLOperationType::Subscription => (
                    GraphQLOperationType::Subscription
                        .to_string()
                        .to_case(Case::Pascal),
                    self.subscription_name
                        .clone()
                        .unwrap_or("Subscription".to_owned()),
                ),
            };

            // collect the required header keys
            let header_keys = sample.headers.as_ref().map(|headers_inner| {
                headers_inner
                    .keys()
                    .map(std::borrow::ToOwned::to_owned)
                    .collect::<BTreeSet<_>>()
            });

            let mut rename_types = HashMap::new();
            rename_types.insert(existing_name, suggested_name);

            // these transformations are required in order to generate a base
            // config.
            GraphQLTypesGenerator::new(sample, type_name_gen)
                .pipe(json::SchemaGenerator::new(
                    &sample.operation_type,
                    header_keys.as_ref(),
                ))
                .pipe(RenameTypes::new(rename_types.into_iter()))
                .transform(config.clone())
        })
        .map(|configs| {
            configs
                .iter()
                .fold(config, |acc, c| acc.merge_right(c.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use gqlforge_valid::Validator;

    use crate::core::config::transformer::Preset;
    use crate::core::generator::generator::test::JsonFixture;
    use crate::core::generator::{FromJsonGenerator, NameGenerator, RequestSample};
    use crate::core::transform::TransformerOps;

    #[tokio::test]
    async fn generate_config_from_json() -> anyhow::Result<()> {
        let mut request_samples = vec![];
        let fixtures = [
            "src/core/generator/tests/fixtures/json/incompatible_properties.json",
            "src/core/generator/tests/fixtures/json/list_incompatible_object.json",
            "src/core/generator/tests/fixtures/json/nested_list.json",
            "src/core/generator/tests/fixtures/json/nested_same_properties.json",
            "src/core/generator/tests/fixtures/json/incompatible_root_object.json",
        ];
        for fixture in fixtures {
            let JsonFixture {
                request,
                response,
                is_mutation,
                is_subscription: _,
                field_name,
            } = JsonFixture::read(fixture).await?;
            let req_sample = RequestSample::new(request.url, response, field_name)
                .with_method(request.method)
                .with_headers(request.headers)
                .with_is_mutation(is_mutation)
                .with_req_body(request.body.unwrap_or_default());

            request_samples.push(req_sample);
        }

        let config = FromJsonGenerator::new(
            &request_samples,
            &NameGenerator::new("T"),
            "Query",
            &None,
            &None,
        )
        .pipe(Preset::default())
        .generate()
        .to_result()?;

        insta::assert_snapshot!(config.to_sdl());
        Ok(())
    }
}
