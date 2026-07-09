use std::borrow::Cow;
use std::hash::{Hash, Hasher};

use derive_setters::Setters;
use gqlforge_hasher::GqlforgeHasher;
use http::header::{HeaderMap, HeaderValue};
use url::Url;

use super::query_encoder::QueryEncoder;
use crate::core::config::Encoding;
use crate::core::endpoint::Endpoint;
use crate::core::has_headers::HasHeaders;
use crate::core::helpers::headers::MustacheHeaders;
use crate::core::ir::DynamicRequest;
use crate::core::ir::model::{CacheKey, IoId};
use crate::core::mustache::{Eval, Mustache, Segment};
use crate::core::path::{PathString, PathValue, ValueString};

/// `RequestTemplate` is an extension of a Mustache template.
/// Various parts of the template can be written as a mustache template.
/// When `to_request` is called, all mustache templates are evaluated.
/// To call `to_request` we need to provide a context.
#[derive(Setters, Debug, Clone)]
pub struct RequestTemplate {
    pub root_url: Mustache,
    pub query: Vec<Query>,
    pub method: reqwest::Method,
    pub headers: MustacheHeaders,
    pub body_path: Option<Mustache>,
    pub endpoint: Endpoint,
    pub encoding: Encoding,
    pub query_encoder: QueryEncoder,
}

#[derive(Setters, Debug, Clone)]
pub struct Query {
    pub key: String,
    pub value: Mustache,
    pub skip_empty: bool,
}

impl RequestTemplate {
    /// Creates a URL for the context
    /// Fills in all the mustache templates with required values.
    fn create_url<C: PathString + PathValue>(&self, ctx: &C) -> anyhow::Result<Url> {
        let mut url = url::Url::parse(self.root_url.render(ctx).as_str())?;
        if self.query.is_empty() && self.root_url.is_const() {
            return Ok(url);
        }

        // evaluates mustache template and returns the values evaluated by mustache
        // template.
        let mustache_eval = ValueStringEval::default();

        let extra_qp = self.query.iter().filter_map(|query| {
            let key = &query.key;
            let value = &query.value;
            let skip = query.skip_empty;
            let parsed_value = mustache_eval.eval(value, ctx);
            if skip && parsed_value.is_none() {
                None
            } else {
                Some(self.query_encoder.encode(key, parsed_value))
            }
        });

        let base_qp = url
            .query_pairs()
            .filter_map(|(k, v)| if v.is_empty() { None } else { Some((k, v)) });

        let qp_string = base_qp.map(|(k, v)| format!("{k}={v}"));
        let qp_string = qp_string.chain(extra_qp).fold(String::new(), |str, item| {
            if str.is_empty() {
                item
            } else if item.is_empty() {
                str
            } else {
                format!("{str}&{item}")
            }
        });

        if qp_string.is_empty() {
            url.set_query(None);
            Ok(url)
        } else {
            url.set_query(Some(qp_string.as_str()));
            Ok(url)
        }
    }

    /// Checks if the template has any mustache templates or not
    /// Returns true if there are not templates
    #[must_use]
    pub fn is_const(&self) -> bool {
        self.root_url.is_const()
            && self.body_path.as_ref().is_none_or(Mustache::is_const)
            && self.query.iter().all(|query| query.value.is_const())
            && self.headers.iter().all(|(_, v)| v.is_const())
    }

    /// Creates a `HeaderMap` for the context
    fn create_headers<C: PathString>(&self, ctx: &C) -> HeaderMap {
        let mut header_map = HeaderMap::new();

        for (k, v) in &self.headers {
            if let Ok(header_value) = HeaderValue::from_str(&v.render(ctx)) {
                header_map.insert(k, header_value);
            }
        }

        header_map
    }

    /// Creates a Request for the given context
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn to_request<C: PathString + HasHeaders + PathValue>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<DynamicRequest<String>> {
        let url = self.create_url(ctx)?;
        let method = self.method.clone();
        let req = reqwest::Request::new(method, url);
        let req = self.set_headers(req, ctx);
        self.set_body(req, ctx)
    }

    /// Sets the body for the request
    fn set_body<C: PathString + HasHeaders>(
        &self,
        mut req: reqwest::Request,
        ctx: &C,
    ) -> anyhow::Result<DynamicRequest<String>> {
        let batching_value = if let Some(body_path) = &self.body_path {
            match &self.encoding {
                Encoding::ApplicationJson => {
                    let (body, batching_value) =
                        ExpressionValueEval::default().eval(body_path, ctx);
                    req.body_mut().replace(body.into());
                    batching_value
                }
                Encoding::ApplicationXWwwFormUrlencoded => {
                    // TODO: this is a performance bottleneck
                    // We first encode everything to string and then back to form-urlencoded
                    let body = body_path.render(ctx);
                    let form_data = match serde_json::from_str::<serde_json::Value>(&body) {
                        Ok(deserialized_data) => serde_urlencoded::to_string(deserialized_data)?,
                        Err(_) => body,
                    };

                    req.body_mut().replace(form_data.into());
                    None
                }
            }
        } else {
            None
        };
        Ok(DynamicRequest::new(req).with_batching_value(batching_value))
    }

    /// Sets the headers for the request
    fn set_headers<C: PathString + HasHeaders>(
        &self,
        mut req: reqwest::Request,
        ctx: &C,
    ) -> reqwest::Request {
        let headers = self.create_headers(ctx);
        if !headers.is_empty() {
            req.headers_mut().extend(headers);
        }

        req.headers_mut().extend(ctx.headers().to_owned());

        // We want to set the header value based on encoding
        // TODO: potential of optimizations.
        // Can set content-type headers while creating the request template
        if self.method != reqwest::Method::GET {
            req.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                match self.encoding {
                    Encoding::ApplicationJson => HeaderValue::from_static("application/json"),
                    Encoding::ApplicationXWwwFormUrlencoded => {
                        HeaderValue::from_static("application/x-www-form-urlencoded")
                    }
                },
            );
        }

        req
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn new(root_url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            root_url: Mustache::parse(root_url),
            query: Vec::new(),
            method: reqwest::Method::GET,
            headers: Vec::new(),
            body_path: None,
            endpoint: Endpoint::new(root_url.to_string()),
            encoding: Encoding::default(),
            query_encoder: QueryEncoder::default(),
        })
    }

    /// Creates a new `RequestTemplate` with the given form encoded URL
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn form_encoded_url(url: &str) -> anyhow::Result<Self> {
        Ok(Self::new(url)?.encoding(Encoding::ApplicationXWwwFormUrlencoded))
    }

    #[must_use]
    pub fn with_body(mut self, body: Mustache) -> Self {
        self.body_path = Some(body);
        self
    }
}

impl TryFrom<Endpoint> for RequestTemplate {
    type Error = anyhow::Error;
    fn try_from(endpoint: Endpoint) -> anyhow::Result<Self> {
        let path = Mustache::parse(endpoint.path.as_str());
        let query = endpoint
            .query
            .iter()
            .map(|(k, v, skip)| {
                Ok(Query {
                    key: k.as_str().to_owned(),
                    value: Mustache::parse(v.as_str()),
                    skip_empty: *skip,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let method = endpoint.method.to_hyper();
        let headers = endpoint
            .headers
            .iter()
            .map(|(k, v)| Ok((k.clone(), Mustache::parse(v.to_str()?))))
            .collect::<anyhow::Result<Vec<_>>>()?;

        let body = endpoint
            .body
            .as_ref()
            .map(|b| Mustache::parse(&b.to_string()));
        let encoding = endpoint.encoding;

        Ok(Self {
            root_url: path,
            query,
            method,
            headers,
            body_path: body,
            endpoint,
            encoding,
            query_encoder: QueryEncoder::default(),
        })
    }
}

impl<Ctx: PathString + HasHeaders + PathValue> CacheKey<Ctx> for RequestTemplate {
    fn cache_key(&self, ctx: &Ctx) -> Option<IoId> {
        let mut hasher = GqlforgeHasher::default();
        let state = &mut hasher;

        self.method.hash(state);

        for (name, mustache) in &self.headers {
            name.hash(state);
            mustache.render(ctx).hash(state);
        }

        for (name, value) in ctx.headers() {
            name.hash(state);
            value.hash(state);
        }

        if let Some(body) = self.body_path.as_ref() {
            body.render(ctx).hash(state);
        }

        let url = self.create_url(ctx).ok()?;
        url.hash(state);

        Some(IoId::new(hasher.finish()))
    }
}

/// `ValueStringEval` parses the mustache template and uses ctx to retrieve the
/// values for templates.
struct ValueStringEval<A>(std::marker::PhantomData<A>);
impl<A> Default for ValueStringEval<A> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<'a, A: PathValue> Eval<'a> for ValueStringEval<A> {
    type In = A;
    type Out = Option<ValueString<'a>>;

    fn eval(&self, mustache: &Mustache, in_value: &'a Self::In) -> Self::Out {
        mustache
            .segments()
            .iter()
            .find_map(|segment| match segment {
                Segment::Literal(text) => Some(ValueString::Value(Cow::Owned(
                    gqlrs::Value::String(text.to_owned()),
                ))),
                Segment::Expression(parts) => in_value.raw_value(parts),
            }) // Return the first value that is found
    }
}

struct ExpressionValueEval<A>(std::marker::PhantomData<A>);
impl<A> Default for ExpressionValueEval<A> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<'a, A: PathString> Eval<'a> for ExpressionValueEval<A> {
    type In = A;
    type Out = (String, Option<String>);

    fn eval(&self, mustache: &Mustache, in_value: &'a Self::In) -> Self::Out {
        let mut result = String::new();
        // This evaluator returns a tuple of (evaluated_string, body_key) where:
        // 1. evaluated_string: The fully rendered template string
        // 2. body_key: The value of the first expression found in the template
        //
        // This implementation is a critical optimization for request batching:
        // - During batching, we need to extract individual request values from the
        //   batch response and map them back to their original requests
        // - Instead of parsing the body JSON multiple times, we extract the key during
        //   initial template evaluation
        // - Since we enforce that batch requests can only contain one expression in
        //   their body, this key uniquely identifies each request
        // - This approach eliminates the need for repeated JSON parsing/serialization
        //   during the batching process, significantly improving performance
        let mut first_expression_value = None;
        for segment in mustache.segments() {
            match segment {
                Segment::Literal(text) => result.push_str(text),
                Segment::Expression(parts) => {
                    if let Some(value) = in_value.path_string(parts) {
                        result.push_str(value.as_ref());
                        if first_expression_value.is_none() {
                            first_expression_value = Some(value.into_owned());
                        }
                    }
                }
            }
        }
        (result, first_expression_value)
    }
}

#[cfg(test)]
mod tests;
