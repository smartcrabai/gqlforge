use std::pin::Pin;

use anyhow::{Result, bail};
use futures_util::{Stream, StreamExt};
use gqlrs_value::ConstValue;
use reqwest::Request;

use crate::core::graphql::SseEventParser;
use crate::core::ir::Error;
use crate::core::runtime::TargetRuntime;

/// Execute an HTTP SSE streaming request and return a stream of decoded JSON
/// values. Unlike `execute_graphql_streaming_request`, this treats each SSE
/// event's `data:` payload as raw JSON (not a GraphQL response wrapper).
///
/// # Errors
///
/// Returns an error if the operation fails.
///
/// # Panics
///
/// Panics if an internal assertion fails.
pub async fn execute_http_streaming_request(
    runtime: &TargetRuntime,
    mut request: Request,
) -> Result<Pin<Box<dyn Stream<Item = Result<ConstValue, Error>> + Send>>> {
    // Add SSE accept header
    request.headers_mut().insert(
        reqwest::header::ACCEPT,
        "text/event-stream"
            .parse()
            .unwrap_or_else(|_| unreachable!("text/event-stream is a valid HeaderValue")),
    );

    let response = runtime.http.execute_raw(request).await?;

    if !response.status().is_success() {
        bail!(
            "HTTP SSE streaming request failed with status: {}",
            response.status()
        );
    }

    let byte_stream = response.bytes_stream();

    let stream = async_stream::stream! {
        let mut parser = SseEventParser::new();

        futures_util::pin_mut!(byte_stream);
        while let Some(chunk_result) = byte_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let events = parser.decode(&chunk);
                    for event_data in events {
                        match parse_sse_raw_json(&event_data) {
                            Ok(value) => yield Ok(value),
                            Err(e) => yield Err(e),
                        }
                    }
                }
                Err(e) => {
                    yield Err(Error::IO(format!("SSE stream error: {e}")));
                    break;
                }
            }
        }
    };

    Ok(Box::pin(stream))
}

/// Parse a single SSE event's data payload as raw JSON.
fn parse_sse_raw_json(event_data: &str) -> Result<ConstValue, Error> {
    let json: serde_json::Value = serde_json::from_str(event_data)
        .map_err(|e| Error::IO(format!("Failed to parse SSE event as JSON: {e}")))?;
    ConstValue::from_json(json)
        .map_err(|e| Error::IO(format!("Failed to convert to ConstValue: {e}")))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn test_parse_sse_raw_json_object() {
        let data = r#"{"temperature": 25, "humidity": 60}"#;
        let result = parse_sse_raw_json(data);
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(
            value,
            ConstValue::from_json(serde_json::json!({"temperature": 25, "humidity": 60})).unwrap()
        );
    }

    #[test]
    fn test_parse_sse_raw_json_number() {
        let result = parse_sse_raw_json("42");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            ConstValue::from_json(serde_json::json!(42)).unwrap()
        );
    }

    #[test]
    fn test_parse_sse_raw_json_string() {
        let result = parse_sse_raw_json(r#""hello""#);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            ConstValue::from_json(serde_json::json!("hello")).unwrap()
        );
    }

    #[test]
    fn test_parse_sse_raw_json_array() {
        let result = parse_sse_raw_json(r"[1, 2, 3]");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            ConstValue::from_json(serde_json::json!([1, 2, 3])).unwrap()
        );
    }

    #[test]
    fn test_parse_sse_raw_json_invalid() {
        let result = parse_sse_raw_json("not json at all");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to parse SSE event as JSON")
        );
    }

    #[test]
    fn test_parse_sse_raw_json_empty() {
        let result = parse_sse_raw_json("");
        assert!(result.is_err());
    }
}
