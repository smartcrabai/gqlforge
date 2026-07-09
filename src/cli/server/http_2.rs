#![allow(clippy::too_many_arguments)]
use std::sync::Arc;

use http::Method;
use http_body_util::{BodyExt, Either, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use rustls_pki_types::CertificateDer;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;

use super::server_config::ServerConfig;
use crate::core::Errata;
use crate::core::config::PrivateKey;
use crate::core::gqlrs_hyper::{GraphQLBatchRequest, GraphQLRequest, GraphQLRequestLike};
use crate::core::http::handle_request;
use crate::core::http::sse::{SseBody, handle_sse_request};

///
/// # Errors
///
/// Returns an error if the operation fails.
///
/// # Panics
///
/// Panics if an internal assertion fails.
#[expect(
    clippy::unwrap_used,
    reason = "http::Response::builder with static status 500 is infallible"
)]
pub async fn start_http_2(
    sc: Arc<ServerConfig>,
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKey,
    server_up_sender: Option<oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    let addr = sc.addr();

    let mut server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(cert, key.into_inner())?;
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind(&addr).await.map_err(Errata::from)?;
    let enable_batch = sc.blueprint.server.enable_batch_requests;

    super::log_launch(sc.as_ref());

    if let Some(sender) = server_up_sender {
        sender
            .send(())
            .or(Err(anyhow::anyhow!("Failed to send message")))?;
    }

    let graphql_endpoint = sc.blueprint.server.routes.graphql().clone();

    loop {
        let (stream, _addr) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let sc = sc.clone();
        let graphql_endpoint = graphql_endpoint.clone();

        tokio::spawn(async move {
            let tls_stream = match tls_acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("TLS handshake error: {}", e);
                    return;
                }
            };
            let io = TokioIo::new(tls_stream);

            let svc = service_fn(move |req: http::Request<Incoming>| {
                let sc = sc.clone();
                let graphql_endpoint = graphql_endpoint.clone();
                async move {
                    let (parts, body) = req.into_parts();
                    let bytes = body.collect().await?.to_bytes();

                    let is_sse = parts.method == Method::POST
                        && parts.uri.path() == graphql_endpoint
                        && serde_json::from_slice::<GraphQLRequest>(&bytes)
                            .is_ok_and(|mut r| r.is_subscription());

                    let req = http::Request::from_parts(parts, Full::new(bytes));

                    if is_sse {
                        match handle_sse_request(req, sc.app_ctx.clone()).await {
                            Ok(resp) => Ok(resp.map(Either::Right)),
                            Err(e) => {
                                tracing::error!("SSE handler error: {}", e);
                                let body =
                                    Full::new(bytes::Bytes::from(format!(r#"{{"error": "{e}"}}"#)));
                                Ok(http::Response::builder()
                                    .status(500)
                                    .body(Either::<Full<bytes::Bytes>, SseBody>::Left(body))
                                    .unwrap())
                            }
                        }
                    } else {
                        let result = if enable_batch {
                            handle_request::<GraphQLBatchRequest>(req, sc.app_ctx.clone()).await
                        } else {
                            handle_request::<GraphQLRequest>(req, sc.app_ctx.clone()).await
                        };
                        result.map(|resp| resp.map(Either::Left))
                    }
                }
            });

            let mut builder = Builder::new(TokioExecutor::new());
            builder.http2();

            if let Err(e) = builder.serve_connection(io, svc).await {
                tracing::error!("Error serving connection: {}", e);
            }
        });
    }
}
