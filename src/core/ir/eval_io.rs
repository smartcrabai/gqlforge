use gqlrs_value::ConstValue;

use super::eval_http::{
    EvalHttp, WorkerContext, execute_grpc_request_with_dl, execute_raw_grpc_request,
    execute_raw_request, execute_request_with_dl, parse_graphql_response, set_headers,
};
use super::model::{CacheKey, IO};
use super::{DynamicRequest, EvalContext, ResolverContextLike};
use crate::core::config::{GraphQLOperationType, PostgresOperation, S3Operation};
use crate::core::data_loader::DataLoader;
use crate::core::graphql::GraphqlDataLoader;
use crate::core::grpc::data_loader::GrpcDataLoader;
use crate::core::http::DataLoaderRequest;
use crate::core::ir::Error;
use crate::core::postgres::request_template::ResultMode;
use crate::core::{grpc, redis};

pub async fn eval_io<Ctx>(io: &IO, ctx: &mut EvalContext<'_, Ctx>) -> Result<ConstValue, Error>
where
    Ctx: ResolverContextLike + Sync,
{
    // Note: Handled the case separately for performance reasons. It avoids
    // cache key generation when it's not required
    let dedupe = io.dedupe();

    if !dedupe || !ctx.is_query() {
        return eval_io_inner(io, ctx).await;
    }
    if let Some(key) = io.cache_key(ctx) {
        ctx.request_ctx
            .cache
            .dedupe(&key, || async {
                ctx.request_ctx
                    .dedupe_handler
                    .dedupe(&key, || eval_io_inner(io, ctx))
                    .await
            })
            .await
    } else {
        eval_io_inner(io, ctx).await
    }
}

#[expect(clippy::too_many_lines, reason = "dispatches all IO resolver variants")]
async fn eval_io_inner<Ctx>(io: &IO, ctx: &mut EvalContext<'_, Ctx>) -> Result<ConstValue, Error>
where
    Ctx: ResolverContextLike + Sync,
{
    match io {
        IO::Http { req_template, dl_id, hook, .. } => {
            let event_worker = &ctx.request_ctx.runtime.cmd_worker;
            let js_worker = &ctx.request_ctx.runtime.worker;
            let eval_http = EvalHttp::new(ctx, req_template, dl_id.as_ref());
            let request = eval_http.init_request()?;
            let response = match (&event_worker, js_worker, hook) {
                (Some(worker), Some(js_worker), Some(hook)) => {
                    let worker_ctx = WorkerContext::new(worker, js_worker, hook);
                    eval_http.execute_with_worker(request, worker_ctx).await?
                }
                _ => eval_http.execute(request).await?,
            };

            Ok(response.body)
        }
        IO::GraphQL { req_template, field_name, dl_id, .. } => {
            let req = req_template.to_request(ctx)?;
            let request = DynamicRequest::new(req);
            let res = if ctx.request_ctx.upstream.batch.is_some()
                && matches!(req_template.operation_type, GraphQLOperationType::Query)
            {
                let data_loader: Option<&DataLoader<DataLoaderRequest, GraphqlDataLoader>> =
                    dl_id.and_then(|dl| ctx.request_ctx.gql_data_loaders.get(dl.as_usize()));
                execute_request_with_dl(ctx, request, data_loader).await?
            } else {
                execute_raw_request(ctx, request).await?
            };

            set_headers(ctx, &res);
            parse_graphql_response(ctx, res, field_name)
        }
        IO::Grpc { req_template, dl_id, hook, .. } => {
            let rendered = req_template.render(ctx)?;
            let worker = &ctx.request_ctx.runtime.worker;

            let res = if ctx.request_ctx.upstream.batch.is_some() &&
                    // TODO: share check for operation_type for resolvers
                    matches!(req_template.operation_type, GraphQLOperationType::Query)
            {
                let data_loader: Option<&DataLoader<grpc::DataLoaderRequest, GrpcDataLoader>> =
                    dl_id.and_then(|index| ctx.request_ctx.grpc_data_loaders.get(index.as_usize()));
                execute_grpc_request_with_dl(ctx, rendered, data_loader).await?
            } else {
                let req = rendered.to_request()?;
                execute_raw_grpc_request(ctx, req, &req_template.operation).await?
            };

            let res = match (worker.as_ref(), hook.as_ref()) {
                (Some(worker), Some(hook)) => hook.on_response(worker, res).await?,
                _ => res,
            };
            set_headers(ctx, &res);

            Ok(res.body)
        }
        IO::GrpcStream { .. } => {
            // GrpcStream is handled by the subscription layer, not eval_io
            Err(Error::IO(
                "GrpcStream should be resolved via subscription stream, not eval_io".to_string(),
            ))
        }
        IO::GraphQLStream { .. } => Err(Error::IO(
            "GraphQLStream should be resolved via subscription stream, not eval_io".to_string(),
        )),
        IO::HttpStream { .. } => Err(Error::IO(
            "HttpStream should be resolved via subscription stream, not eval_io".to_string(),
        )),
        IO::PostgresStream { .. } => Err(Error::IO(
            "PostgresStream should be resolved via subscription stream, not eval_io".to_string(),
        )),
        IO::RedisStream { .. } => Err(Error::IO(
            "RedisStream should be resolved via subscription stream, not eval_io".to_string(),
        )),
        IO::Postgres { req_template, dl_id: _, connection_id, .. } => {
            let rendered = req_template
                .render(ctx)
                .map_err(|e| Error::IO(e.to_string()))?;
            let pg = ctx
                .request_ctx
                .runtime
                .postgres
                .get(connection_id)
                .ok_or_else(|| {
                    Error::IO(format!(
                        "PostgreSQL connection '{connection_id}' not configured"
                    ))
                })?;
            let result = match req_template.result_mode {
                ResultMode::Rows => pg.execute(&rendered.sql, &rendered.params).await,
                ResultMode::AffectedRows => pg
                    .execute_affected(&rendered.sql, &rendered.params)
                    .await
                    .map(|count| ConstValue::Number(count.into())),
            }
            .map_err(|e| Error::IO(e.to_string()))?;
            // SELECT_ONE: returns the first element of the list (Null if empty)
            if req_template.operation == PostgresOperation::SelectOne {
                if let ConstValue::List(vec) = result {
                    Ok(vec.into_iter().next().unwrap_or(ConstValue::Null))
                } else {
                    Ok(result)
                }
            } else {
                Ok(result)
            }
        }
        IO::Redis { req_template, connection_id, .. } => {
            let rendered = req_template
                .render(ctx)
                .map_err(|e| Error::IO(e.to_string()))?;
            let redis = ctx
                .request_ctx
                .runtime
                .redis
                .get(connection_id)
                .ok_or_else(|| {
                    Error::IO(format!("Redis connection '{connection_id}' not configured"))
                })?;
            let result = redis
                .execute(&rendered.command, &rendered.args)
                .await
                .map_err(|e| Error::IO(e.to_string()))?;
            // Correct for RESP2/RESP3 shape differences and wire-level types
            // that don't match the directive's documented return type
            // (e.g. EXISTS/SET -> Boolean) before interpreting string
            // leaves as JSON.
            let result = redis::normalize_command_result(&req_template.operation, result);
            Ok(redis::decode_value_leaves(
                result,
                &req_template.payload_type,
            ))
        }
        IO::Js { name } => {
            match ctx
                .request_ctx
                .runtime
                .worker
                .as_ref()
                .zip(ctx.value().cloned())
            {
                Some((worker, value)) => {
                    let val = worker.call(name, value).await?;
                    Ok(val.unwrap_or_default())
                }
                _ => Ok(ConstValue::Null),
            }
        }
        IO::S3 { req_template, .. } => {
            let rendered = req_template.render(ctx);
            if rendered.bucket.is_empty() {
                return Err(Error::IO("S3 bucket name must not be empty".to_string()));
            }
            let link_id = rendered.link_id.as_deref();
            let s3 = match link_id {
                Some(id) => ctx
                    .request_ctx
                    .runtime
                    .s3
                    .get(id)
                    .ok_or_else(|| Error::IO(format!("S3 link '{id}' not found in runtime")))?,
                None => ctx
                    .request_ctx
                    .runtime
                    .s3
                    .get("")
                    .or_else(|| ctx.request_ctx.runtime.s3.values().next())
                    .ok_or_else(|| Error::IO("S3 runtime not configured".to_string()))?,
            };

            match rendered.operation {
                S3Operation::GetPresignedUrl => {
                    let key = rendered.key.as_deref().ok_or_else(|| {
                        Error::IO("S3 GET_PRESIGNED_URL requires a key".to_string())
                    })?;
                    let url = s3
                        .get_presigned_url(&rendered.bucket, key, rendered.expiration)
                        .await
                        .map_err(|e| Error::IO(e.to_string()))?;
                    Ok(ConstValue::String(url))
                }
                S3Operation::PutPresignedUrl => {
                    let key = rendered.key.as_deref().ok_or_else(|| {
                        Error::IO("S3 PUT_PRESIGNED_URL requires a key".to_string())
                    })?;
                    let url = s3
                        .put_presigned_url(
                            &rendered.bucket,
                            key,
                            rendered.expiration,
                            rendered.content_type.as_deref(),
                        )
                        .await
                        .map_err(|e| Error::IO(e.to_string()))?;
                    Ok(ConstValue::String(url))
                }
                S3Operation::List => {
                    let result = s3
                        .list_objects(&rendered.bucket, rendered.prefix.as_deref())
                        .await
                        .map_err(|e| Error::IO(e.to_string()))?;
                    Ok(result)
                }
                S3Operation::Delete => {
                    let key = rendered
                        .key
                        .as_deref()
                        .ok_or_else(|| Error::IO("S3 DELETE requires a key".to_string()))?;
                    let result = s3
                        .delete_object(&rendered.bucket, key)
                        .await
                        .map_err(|e| Error::IO(e.to_string()))?;
                    Ok(result)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::sync::{Arc, Mutex};

    use crate::core::postgres::PostgresIO;

    enum AffectedRowsResult {
        Count(u64),
        Error,
    }

    struct AffectedRowsPostgres {
        result: AffectedRowsResult,
        requests: Mutex<Vec<(String, Vec<Option<String>>)>>,
    }

    impl AffectedRowsPostgres {
        fn new(result: AffectedRowsResult) -> Self {
            Self { result, requests: Mutex::new(vec![]) }
        }
    }

    #[async_trait::async_trait]
    impl PostgresIO for AffectedRowsPostgres {
        async fn execute(
            &self,
            _query: &str,
            _params: &[Option<String>],
        ) -> anyhow::Result<ConstValue> {
            unreachable!("affected-row mode must not request rows")
        }

        async fn execute_affected(
            &self,
            query: &str,
            params: &[Option<String>],
        ) -> anyhow::Result<u64> {
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((query.to_string(), params.to_vec()));
            match self.result {
                AffectedRowsResult::Count(count) => Ok(count),
                AffectedRowsResult::Error => anyhow::bail!("database unavailable"),
            }
        }
    }

    fn affected_rows_io() -> IO {
        IO::Postgres {
            req_template: crate::core::postgres::RequestTemplate {
                table: "metrics".to_string(),
                operation: PostgresOperation::Insert,
                filter: None,
                input: Some(crate::core::mustache::Mustache::parse(
                    r#"{"host":"api-1"}"#,
                )),
                limit: None,
                offset: None,
                order_by: None,
                columns: vec!["host".to_string()],
                result_mode: ResultMode::AffectedRows,
            },
            group_by: None,
            dl_id: None,
            dedupe: false,
            connection_id: "metrics".to_string(),
        }
    }

    #[tokio::test]
    async fn postgres_affected_row_mode_returns_a_number() {
        let io = affected_rows_io();
        let mut runtime = crate::cli::runtime::init(&Blueprint::default()).unwrap();
        let postgres = Arc::new(AffectedRowsPostgres::new(AffectedRowsResult::Count(1)));
        runtime
            .postgres
            .insert("metrics".to_string(), postgres.clone());
        let req_ctx = RequestContext::new(runtime);
        let res_ctx = EmptyResolverContext {};
        let mut eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let result = eval_io(&io, &mut eval_ctx).await.unwrap();

        assert_eq!(result, ConstValue::Number(1.into()));
        assert_eq!(
            postgres
                .requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            [(
                "INSERT INTO \"metrics\" (\"host\") VALUES ($1)".to_string(),
                vec![Some("api-1".to_string())],
            )]
        );
    }

    #[tokio::test]
    async fn postgres_affected_row_mode_preserves_zero() {
        let mut runtime = crate::cli::runtime::init(&Blueprint::default()).unwrap();
        runtime.postgres.insert(
            "metrics".to_string(),
            Arc::new(AffectedRowsPostgres::new(AffectedRowsResult::Count(0))),
        );
        let req_ctx = RequestContext::new(runtime);
        let res_ctx = EmptyResolverContext {};
        let mut eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        assert_eq!(
            eval_io(&affected_rows_io(), &mut eval_ctx).await.unwrap(),
            ConstValue::Number(0.into())
        );
    }

    #[tokio::test]
    async fn postgres_affected_row_mode_propagates_errors() {
        let mut runtime = crate::cli::runtime::init(&Blueprint::default()).unwrap();
        runtime.postgres.insert(
            "metrics".to_string(),
            Arc::new(AffectedRowsPostgres::new(AffectedRowsResult::Error)),
        );
        let req_ctx = RequestContext::new(runtime);
        let res_ctx = EmptyResolverContext {};
        let mut eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let error = eval_io(&affected_rows_io(), &mut eval_ctx)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("database unavailable"));
    }
    use super::*;
    use crate::core::blueprint::Blueprint;
    use crate::core::config::PostgresPayloadType;
    use crate::core::http::RequestContext;
    use crate::core::ir::EmptyResolverContext;

    #[tokio::test]
    async fn postgres_stream_eval_io_returns_error() {
        let io = IO::PostgresStream {
            connection_id: "main".to_string(),
            channel: "users_changes".to_string(),
            payload_type: PostgresPayloadType::Json,
        };
        let runtime = crate::cli::runtime::init(&Blueprint::default()).unwrap();
        let req_ctx = RequestContext::new(runtime);
        let res_ctx = EmptyResolverContext {};
        let mut eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let result = eval_io(&io, &mut eval_ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("PostgresStream should be resolved via subscription"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn redis_stream_eval_io_returns_error() {
        let io = IO::RedisStream {
            connection_id: "main".to_string(),
            source: crate::core::redis::RedisStreamSource::PubSub {
                channel: crate::core::mustache::Mustache::parse("events"),
            },
            payload_type: crate::core::config::RedisPayloadType::Json,
        };
        let runtime = crate::cli::runtime::init(&Blueprint::default()).unwrap();
        let req_ctx = RequestContext::new(runtime);
        let res_ctx = EmptyResolverContext {};
        let mut eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let result = eval_io(&io, &mut eval_ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("RedisStream should be resolved via subscription"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn redis_connection_not_configured_returns_error() {
        let io = IO::Redis {
            req_template: crate::core::redis::request_template::RequestTemplate {
                operation: crate::core::config::RedisOperation::Get,
                key: Some(crate::core::mustache::Mustache::parse("user:1")),
                field: None,
                value: None,
                ttl: None,
                start: None,
                stop: None,
                channel: None,
                payload_type: crate::core::config::RedisPayloadType::Json,
            },
            dedupe: false,
            connection_id: "main".to_string(),
        };
        let runtime = crate::cli::runtime::init(&Blueprint::default()).unwrap();
        let req_ctx = RequestContext::new(runtime);
        let res_ctx = EmptyResolverContext {};
        let mut eval_ctx = EvalContext::new(&req_ctx, &res_ctx);

        let result = eval_io(&io, &mut eval_ctx).await;
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Redis connection 'main' not configured"),
            "unexpected error: {err}"
        );
    }
}
