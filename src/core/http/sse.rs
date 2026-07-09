use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use http::{Response, StatusCode, header};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;

use crate::core::app_context::AppContext;
use crate::core::http::RequestContext;

pub type SseBody = StreamBody<
    futures_util::stream::BoxStream<'static, Result<Frame<Bytes>, std::convert::Infallible>>,
>;

/// Handle an SSE subscription request.
///
/// Parses the GraphQL subscription query from the POST body,
/// executes it as a stream, and returns each result as an SSE `data:` event.
///
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn handle_sse_request(
    req: http::Request<Full<Bytes>>,
    app_ctx: Arc<AppContext>,
) -> anyhow::Result<Response<SseBody>> {
    let body_bytes = req.into_body().collect().await?.to_bytes();
    let graphql_req: gqlrs::Request = serde_json::from_slice(&body_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to parse GraphQL request: {e}"))?;

    let req_ctx = Arc::new(RequestContext::from(app_ctx.as_ref()));
    let graphql_req = graphql_req.data(req_ctx);

    let stream = app_ctx.schema.execute_stream(graphql_req);

    let sse_stream = stream.map(|response| {
        let json = serde_json::to_string(&response).unwrap_or_default();
        let event = format!("data: {json}\n\n");
        Ok(Frame::data(Bytes::from(event)))
    });

    let body = StreamBody::new(Box::pin(sse_stream)
        as futures_util::stream::BoxStream<
            'static,
            Result<Frame<Bytes>, std::convert::Infallible>,
        >);

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(body)?;

    Ok(response)
}
