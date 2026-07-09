use anyhow::Result;
use bytes::Bytes;
use derive_setters::Setters;
use gqlrs_value::{ConstValue, Name};
use http_body_util::Full;
use indexmap::IndexMap;
use prost::Message;
use tonic::Status;
use tonic_types::Status as GrpcStatus;

use crate::core::grpc::protobuf::ProtobufOperation;
use crate::core::ir::Error;

#[derive(Clone, Debug, Default, Setters)]
pub struct Response<Body> {
    pub status: reqwest::StatusCode,
    pub headers: headers::HeaderMap,
    pub body: Body,
}

// Trait to convert a serde_json_borrow::Value to a ConstValue.
// serde_json_borrow::Value is a borrowed version of serde_json::Value.
// It has a limited lifetime tied to the input JSON, making it more
// efficient. Benchmarking is required to determine the performance If any
// change is made.

pub trait FromValue {
    fn from_value(value: serde_json_borrow::Value) -> Self;
}

impl FromValue for ConstValue {
    fn from_value(value: serde_json_borrow::Value) -> Self {
        match value {
            serde_json_borrow::Value::Null => ConstValue::Null,
            serde_json_borrow::Value::Bool(b) => ConstValue::Boolean(b),
            serde_json_borrow::Value::Number(n) => ConstValue::Number(n.into()),
            serde_json_borrow::Value::Str(s) => ConstValue::String(s.into()),
            serde_json_borrow::Value::Array(a) => {
                ConstValue::List(a.into_iter().map(|v| Self::from_value(v)).collect())
            }
            serde_json_borrow::Value::Object(o) => ConstValue::Object(
                o.into_vec()
                    .into_iter()
                    .map(|(k, v)| (Name::new(k), Self::from_value(v)))
                    .collect(),
            ),
        }
    }
}

impl Response<Bytes> {
    /// Handle error responses with body extraction for better error messages
    /// This is a common pattern used across different HTTP clients
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn from_reqwest_with_error_handling(
        response: reqwest::Response,
    ) -> anyhow::Result<Self> {
        // Check if it's an error status
        if let Err(err) = response.error_for_status_ref() {
            // Get the body content first (this is the key step)
            let body_text = response.text().await?;
            // Create an error with the status code and add body content as context
            let err = Error::HTTP {
                message: err.without_url().to_string(),
                body: body_text.clone(),
            };
            return Err(anyhow::Error::new(err).context(body_text));
        }

        // If not an error status, proceed normally
        Self::from_reqwest(response).await
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn from_reqwest(resp: reqwest::Response) -> Result<Self> {
        let status = resp.status();
        let headers = resp.headers().to_owned();
        let body = resp.bytes().await?;
        Ok(Response { status, headers, body })
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub async fn from_hyper(resp: http::Response<Full<Bytes>>) -> Result<Self> {
        use http_body_util::BodyExt;
        let status = resp.status();
        let headers = resp.headers().to_owned();
        let body = resp.into_body().collect().await?.to_bytes();
        Ok(Response { status, headers, body })
    }

    #[must_use]
    pub fn empty() -> Self {
        Response {
            status: reqwest::StatusCode::OK,
            headers: headers::HeaderMap::default(),
            body: Bytes::new(),
        }
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn to_json<T: Default + FromValue>(self) -> Result<Response<T>> {
        if self.body.is_empty() {
            return Ok(Response {
                status: self.status,
                headers: self.headers,
                body: Default::default(),
            });
        }
        // Note: We convert the body to a serde_json_borrow::Value for better
        // performance. Warning: Do not change this to direct conversion to `T`
        // without benchmarking the performance impact.
        let body: serde_json_borrow::Value = serde_json::from_slice(&self.body)?;
        let body = T::from_value(body);
        Ok(Response { status: self.status, headers: self.headers, body })
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn to_grpc_value(self, operation: &ProtobufOperation) -> Result<Response<gqlrs::Value>> {
        let mut resp = Response::default();
        let body = operation.convert_output::<gqlrs::Value>(&self.body)?;
        resp.body = body;
        resp.status = self.status;
        resp.headers = self.headers;
        Ok(resp)
    }

    pub fn to_grpc_error(&self, operation: &ProtobufOperation) -> anyhow::Error {
        let Some(grpc_status) = Status::from_header_map(&self.headers) else {
            return Error::IO("Error while parsing upstream headers".to_owned()).into();
        };

        let mut obj: IndexMap<Name, gqlrs::Value> = IndexMap::new();
        let mut status_details = Vec::new();
        if !grpc_status.details().is_empty() {
            if let Ok(status) = GrpcStatus::decode(grpc_status.details()) {
                obj.insert(Name::new("code"), status.code.into());
                obj.insert(Name::new("message"), status.message.clone().into());

                for detail in status.details {
                    let type_url = &detail.type_url;
                    let type_name = type_url.split('/').next_back().unwrap_or("");

                    match operation.find_message(type_name) {
                        Some(message) => match message.decode(detail.value.as_slice()) {
                            Ok(decoded) => {
                                status_details.push(decoded);
                            }
                            _ => {
                                tracing::error!("Error while decoding message: {type_name}");
                            }
                        },
                        _ => {
                            tracing::error!(
                                "Error while searching descriptor for message: {type_name}"
                            );
                        }
                    }
                }
            } else {
                tracing::error!("Error while decoding gRPC status details");
            }
        }
        obj.insert(Name::new("details"), ConstValue::List(status_details));

        let error = Error::GRPC {
            grpc_code: grpc_status.code() as i32,
            grpc_description: grpc_status.code().description().to_owned(),
            grpc_status_message: grpc_status.message().to_owned(),
            grpc_status_details: ConstValue::Object(obj),
        };

        // TODO: because of this conversion to anyhow::Error
        // we lose additional details that could be added
        // through gqlrs::ErrorExtensions
        anyhow::Error::new(error)
    }

    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn to_resp_string(self) -> Result<Response<String>> {
        Ok(Response::<String> {
            body: String::from_utf8(self.body.to_vec())?,
            status: self.status,
            headers: self.headers,
        })
    }
}

impl From<Response<Bytes>> for http::Response<Full<Bytes>> {
    fn from(resp: Response<Bytes>) -> Self {
        let mut response = http::Response::new(Full::new(resp.body));
        *response.headers_mut() = resp.headers;
        *response.status_mut() = resp.status;
        response
    }
}
