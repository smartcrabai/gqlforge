use std::sync::Arc;

use dashmap::DashMap;
use gqlrs::dynamic::{self, DynamicRequest};
use gqlrs_value::ConstValue;

use super::jit::AnyResponse;
use crate::core::blueprint::{Blueprint, Definition, SchemaModifiers};
use crate::core::data_loader::{DataLoader, DedupeResult};
use crate::core::gqlrs_hyper::OperationId;
use crate::core::graphql::GraphqlDataLoader;
use crate::core::grpc;
use crate::core::grpc::data_loader::GrpcDataLoader;
use crate::core::http::{DataLoaderRequest, HttpDataLoader};
use crate::core::ir::Error;
use crate::core::ir::model::{DataLoaderId, IO, IR, IoId};
use crate::core::jit::{OPHash, OperationPlan};
use crate::core::rest::{Checked, EndpointSet};
use crate::core::runtime::TargetRuntime;

pub struct AppContext {
    pub schema: dynamic::Schema,
    pub runtime: TargetRuntime,
    pub blueprint: Blueprint,
    pub http_data_loaders: Arc<Vec<DataLoader<DataLoaderRequest, HttpDataLoader>>>,
    pub gql_data_loaders: Arc<Vec<DataLoader<DataLoaderRequest, GraphqlDataLoader>>>,
    pub grpc_data_loaders: Arc<Vec<DataLoader<grpc::DataLoaderRequest, GrpcDataLoader>>>,
    pub endpoints: EndpointSet<Checked>,
    pub dedupe_handler: Arc<DedupeResult<IoId, ConstValue, Error>>,
    pub dedupe_operation_handler: DedupeResult<OperationId, AnyResponse<Vec<u8>>, Error>,
    pub operation_plans: DashMap<OPHash, OperationPlan<gqlrs_value::Value>>,
    pub const_execution_cache: DashMap<OPHash, AnyResponse<Vec<u8>>>,
}

impl AppContext {
    #[expect(clippy::too_many_lines, reason = "complex initialization logic")]
    pub fn new(
        mut blueprint: Blueprint,
        runtime: TargetRuntime,
        endpoints: EndpointSet<Checked>,
    ) -> Self {
        let mut http_data_loaders = vec![];
        let mut gql_data_loaders = vec![];
        let mut grpc_data_loaders = vec![];

        for def in &mut blueprint.definitions {
            if let Definition::Object(def) = def {
                for field in &mut def.fields {
                    let upstream_batch = &blueprint.upstream.batch;
                    field.map_expr(|expr| {
                        expr.modify(&mut |expr| match expr {
                            IR::IO(io) => match io.as_ref() {
                                IO::Http {
                                    req_template, group_by, is_list, dedupe, hook, ..
                                } => {
                                    let is_list = *is_list;
                                    let dedupe = *dedupe;
                                    let data_loader = HttpDataLoader::new(
                                        runtime.clone(),
                                        group_by.clone(),
                                        is_list,
                                    )
                                    .to_data_loader(&upstream_batch.clone().unwrap_or_default());

                                    let result = Some(IR::IO(Box::new(IO::Http {
                                        req_template: req_template.clone(),
                                        group_by: group_by.clone(),
                                        dl_id: Some(DataLoaderId::new(http_data_loaders.len())),
                                        hook: hook.clone(),
                                        is_list,
                                        dedupe,
                                    })));

                                    http_data_loaders.push(data_loader);

                                    result
                                }

                                IO::GraphQL { req_template, field_name, batch, dedupe, .. } => {
                                    let dedupe = *dedupe;
                                    let graphql_data_loader =
                                        GraphqlDataLoader::new(runtime.clone(), *batch)
                                            .into_data_loader(
                                                &upstream_batch.clone().unwrap_or_default(),
                                            );

                                    let result = Some(IR::IO(Box::new(IO::GraphQL {
                                        req_template: req_template.clone(),
                                        field_name: field_name.clone(),
                                        batch: *batch,
                                        dl_id: Some(DataLoaderId::new(gql_data_loaders.len())),
                                        dedupe,
                                    })));

                                    gql_data_loaders.push(graphql_data_loader);

                                    result
                                }

                                IO::Grpc { req_template, group_by, dedupe, hook, .. } => {
                                    let dedupe = *dedupe;
                                    let data_loader = GrpcDataLoader {
                                        runtime: runtime.clone(),
                                        operation: req_template.operation.clone(),
                                        group_by: group_by.clone(),
                                    };
                                    let data_loader = data_loader.into_data_loader(
                                        &upstream_batch.clone().unwrap_or_default(),
                                    );

                                    let result = Some(IR::IO(Box::new(IO::Grpc {
                                        req_template: req_template.clone(),
                                        group_by: group_by.clone(),
                                        dl_id: Some(DataLoaderId::new(grpc_data_loaders.len())),
                                        dedupe,
                                        hook: hook.clone(),
                                    })));

                                    grpc_data_loaders.push(data_loader);

                                    result
                                }
                                IO::GrpcStream { .. } => {
                                    // GrpcStream is used in subscriptions, no
                                    // data loader needed
                                    None
                                }
                                IO::GraphQLStream { .. } => {
                                    // GraphQLStream is used in subscriptions,
                                    // no data loader needed
                                    None
                                }
                                IO::HttpStream { .. } => {
                                    // HttpStream is used in subscriptions, no
                                    // data loader needed
                                    None
                                }
                                IO::PostgresStream { .. } => {
                                    // PostgresStream is used in subscriptions,
                                    // no data loader
                                    // needed
                                    None
                                }
                                IO::RedisStream { .. } => {
                                    // RedisStream is used in subscriptions, no
                                    // data loader needed
                                    None
                                }
                                IO::Postgres {
                                    req_template,
                                    group_by,
                                    dedupe,
                                    connection_id,
                                    ..
                                } => {
                                    // For now, Postgres data loaders are not
                                    // yet batched in the same way as HTTP/gRPC.
                                    // We just carry the IO through.
                                    Some(IR::IO(Box::new(IO::Postgres {
                                        req_template: req_template.clone(),
                                        group_by: group_by.clone(),
                                        dl_id: None,
                                        dedupe: *dedupe,
                                        connection_id: connection_id.clone(),
                                    })))
                                }
                                IO::Redis { req_template, dedupe, connection_id } => {
                                    // Redis has no data loader (no batching
                                    // yet); carry the IO
                                    // through unchanged.
                                    Some(IR::IO(Box::new(IO::Redis {
                                        req_template: req_template.clone(),
                                        dedupe: *dedupe,
                                        connection_id: connection_id.clone(),
                                    })))
                                }
                                IO::Js { name: method } => {
                                    Some(IR::IO(Box::new(IO::Js { name: method.clone() })))
                                }
                                IO::S3 { req_template, dedupe } => Some(IR::IO(Box::new(IO::S3 {
                                    req_template: req_template.clone(),
                                    dedupe: *dedupe,
                                }))),
                            },
                            _ => None,
                        })
                    });
                }
            }
        }

        let schema = blueprint
            .to_schema_with(&SchemaModifiers::default().extensions(runtime.extensions.clone()));

        AppContext {
            schema,
            runtime,
            blueprint,
            http_data_loaders: Arc::new(http_data_loaders),
            gql_data_loaders: Arc::new(gql_data_loaders),
            grpc_data_loaders: Arc::new(grpc_data_loaders),
            endpoints,

            dedupe_handler: Arc::new(DedupeResult::new(false)),
            dedupe_operation_handler: DedupeResult::new(false),
            operation_plans: DashMap::new(),
            const_execution_cache: DashMap::default(),
        }
    }

    pub async fn execute(&self, request: impl Into<DynamicRequest>) -> gqlrs::Response {
        self.schema.execute(request).await
    }
}
