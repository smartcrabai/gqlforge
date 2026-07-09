use std::collections::BTreeMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::FuturesOrdered;
use gqlforge_hasher::GqlforgeHasher;
use gqlrs::{BatchRequest, Value};
use gqlrs_value::{ConstValue, Extensions};

use super::{AnyResponse, BatchResponse, Response};
use crate::core::app_context::AppContext;
use crate::core::gqlrs_hyper::OperationId;
use crate::core::http::RequestContext;
use crate::core::jit::{self, ConstValueExecutor, OPHash, Pos, Positioned};

#[derive(Clone)]
pub struct JITExecutor {
    app_ctx: Arc<AppContext>,
    req_ctx: Arc<RequestContext>,
    operation_id: OperationId,
}

impl JITExecutor {
    pub fn new(
        app_ctx: Arc<AppContext>,
        req_ctx: Arc<RequestContext>,
        operation_id: OperationId,
    ) -> Self {
        Self { app_ctx, req_ctx, operation_id }
    }

    #[inline]
    async fn exec(
        &self,
        exec: ConstValueExecutor,
        jit_request: jit::Request<ConstValue>,
    ) -> AnyResponse<Vec<u8>> {
        exec.execute(&self.app_ctx, &self.req_ctx, jit_request)
            .await
    }

    #[inline]
    async fn dedupe_and_exec(
        &self,
        exec: ConstValueExecutor,
        jit_request: jit::Request<ConstValue>,
    ) -> AnyResponse<Vec<u8>> {
        let out = self
            .app_ctx
            .dedupe_operation_handler
            .dedupe(&self.operation_id, || {
                Box::pin(async move {
                    let resp = self.exec(exec, jit_request).await;
                    Ok(resp)
                })
            })
            .await;

        out.unwrap_or_default()
    }

    #[inline]
    fn req_hash(request: &gqlrs::Request) -> OPHash {
        let mut hasher = GqlforgeHasher::default();
        request.query.hash(&mut hasher);

        OPHash::new(hasher.finish())
    }
}

impl JITExecutor {
    pub fn execute(
        &self,
        request: gqlrs::Request,
    ) -> impl Future<Output = AnyResponse<Vec<u8>>> + Send + '_ {
        // TODO: hash considering only the query itself ignoring specified operation and
        // variables that could differ for the same query
        let hash = Self::req_hash(&request);

        async move {
            if let Some(response) = self.app_ctx.const_execution_cache.get(&hash) {
                return response.clone();
            }

            let jit_request = jit::Request::from(request);
            let exec = if let Some(op) = self.app_ctx.operation_plans.get(&hash) {
                ConstValueExecutor::from(op.value().clone())
            } else {
                let exec = match ConstValueExecutor::try_new(&jit_request, &self.app_ctx) {
                    Ok(exec) => exec,
                    Err(error) => {
                        return Response::<gqlrs::Value>::default()
                            .with_errors(vec![Positioned::new(error, Pos::default())])
                            .into();
                    }
                };
                self.app_ctx
                    .operation_plans
                    .insert(hash.clone(), exec.plan.clone());
                exec
            };

            let is_const = exec.plan.is_const;
            let is_protected = exec.plan.is_protected;

            let response = if exec.plan.can_dedupe() {
                self.dedupe_and_exec(exec, jit_request).await
            } else {
                self.exec(exec, jit_request).await
            };

            // Cache the response if it's constant and not wrapped with protected.
            if is_const && !is_protected {
                self.app_ctx
                    .const_execution_cache
                    .insert(hash, response.clone());
            }

            response
        }
    }

    /// Execute a GraphQL batch query.
    pub async fn execute_batch(&self, batch_request: BatchRequest) -> BatchResponse<Vec<u8>> {
        match batch_request {
            BatchRequest::Single(request) => BatchResponse::Single(self.execute(request).await),
            BatchRequest::Batch(requests) => {
                let futs: FuturesOrdered<_> = requests
                    .into_iter()
                    .map(|request| self.execute(request))
                    .collect();
                let responses = futs.collect::<Vec<_>>().await;
                BatchResponse::Batch(responses)
            }
        }
    }
}

// TODO: used only for introspection, simplify somehow?
impl From<jit::Request<Value>> for gqlrs::Request {
    fn from(value: jit::Request<Value>) -> Self {
        let mut request = gqlrs::Request::new(value.query);
        request.variables.extend(
            value
                .variables
                .into_hashmap()
                .into_iter()
                .map(|(k, v)| (gqlrs::Name::new(k), v))
                .collect::<BTreeMap<_, _>>(),
        );
        request.extensions = Extensions(value.extensions);
        request.operation_name = value.operation_name;
        request
    }
}
