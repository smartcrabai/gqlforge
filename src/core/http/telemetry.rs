use anyhow::Result;
use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use opentelemetry_http::HeaderExtractor;
use opentelemetry_semantic_conventions::trace::{
    HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, HTTP_ROUTE, URL_PATH,
};

use crate::core::blueprint::telemetry::Telemetry;

static HTTP_SERVER_REQUEST_COUNT: std::sync::LazyLock<Counter<u64>> =
    std::sync::LazyLock::new(|| {
        let meter = opentelemetry::global::meter("http_request");

        meter
            .u64_counter("http.server.request.count")
            .with_description("Number of incoming request handled")
            .build()
    });

#[derive(Default)]
pub struct RequestCounter {
    attributes: Option<Vec<KeyValue>>,
}

impl RequestCounter {
    pub fn new(telemetry: &Telemetry, req: &Request<Full<Bytes>>) -> Self {
        if telemetry.export.is_none() {
            return Self::default();
        }

        let observable_headers = &telemetry.request_headers;
        let headers = req.headers();
        let mut attributes = Vec::with_capacity(observable_headers.len() + 3);

        attributes.push(KeyValue::new(URL_PATH, req.uri().path().to_string()));
        attributes.push(KeyValue::new(HTTP_REQUEST_METHOD, req.method().to_string()));

        for name in observable_headers {
            if let Some(value) = headers.get(name) {
                attributes.push(KeyValue::new(
                    format!("http.request.header.{name}"),
                    format!("{value:?}"),
                ));
            }
        }

        Self { attributes: Some(attributes) }
    }

    pub fn set_http_route(&mut self, route: &str) {
        if let Some(ref mut attributes) = self.attributes {
            attributes.push(KeyValue::new(HTTP_ROUTE, route.to_string()));
        }
    }

    pub fn update(self, response: &Result<Response<Full<Bytes>>>) {
        if let Some(mut attributes) = self.attributes {
            if let Ok(response) = response {
                attributes.push(get_response_status_code(response));
            }
            HTTP_SERVER_REQUEST_COUNT.add(1, &attributes);
        }
    }
}

pub fn get_response_status_code(response: &Response<Full<Bytes>>) -> KeyValue {
    KeyValue::new(
        HTTP_RESPONSE_STATUS_CODE,
        i64::from(response.status().as_u16()),
    )
}

pub fn propagate_context(req: &Request<Full<Bytes>>) {
    let _context = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(req.headers()))
    });

    // TEMPORARY: Disabled due to opentelemetry version incompatibility
    // tracing-opentelemetry v0.32.1 uses opentelemetry v0.31, but we use v0.32
    // TODO: Re-enable when tracing-opentelemetry supports opentelemetry v0.32
    // let _ = tracing::Span::current().set_parent(context);
}
