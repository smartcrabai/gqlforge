use std::pin::Pin;

use anyhow::{Result, bail};
use futures_util::{Stream, StreamExt};
use gqlrs_value::ConstValue;
use reqwest::Request;

use super::sse_client::SseEventParser;
use crate::core::ir::Error;
use crate::core::runtime::TargetRuntime;

/// Execute a GraphQL subscription request against an upstream SSE endpoint
/// and return a stream of decoded field values.
///
/// # Errors
///
/// Returns an error if the operation fails.
///
/// # Panics
///
/// Panics if an internal assertion fails.
pub async fn execute_graphql_streaming_request(
    runtime: &TargetRuntime,
    stream_url: &str,
    mut request: Request,
    field_name: &str,
) -> Result<Pin<Box<dyn Stream<Item = Result<ConstValue, Error>> + Send>>> {
    // Replace the URL with the SSE stream endpoint
    *request.url_mut() = stream_url
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid stream URL: {e}"))?;

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
            "GraphQL SSE streaming request failed with status: {}",
            response.status()
        );
    }

    let field_name = field_name.to_string();
    let byte_stream = response.bytes_stream();

    let stream = async_stream::stream! {
        let mut parser = SseEventParser::new();

        futures_util::pin_mut!(byte_stream);
        while let Some(chunk_result) = byte_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let events = parser.decode(&chunk);
                    for event_data in events {
                        match parse_sse_graphql_event(&event_data, &field_name) {
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

/// Parse a single SSE event's data payload as a GraphQL response and extract
/// the specified field.
fn parse_sse_graphql_event(event_data: &str, field_name: &str) -> Result<ConstValue, Error> {
    let json: serde_json::Value = serde_json::from_str(event_data)
        .map_err(|e| Error::IO(format!("Failed to parse SSE event as JSON: {e}")))?;

    // Check for GraphQL errors
    if let Some(errors) = json.get("errors")
        && let Some(arr) = errors.as_array()
        && !arr.is_empty()
    {
        let msg = arr
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::IO(format!("GraphQL subscription error: {msg}")));
    }

    let data = json
        .get("data")
        .ok_or_else(|| Error::IO("SSE event missing 'data' field in GraphQL response".into()))?;

    let field_value = data.get(field_name).ok_or_else(|| {
        Error::IO(format!(
            "SSE event missing field '{field_name}' in GraphQL response data"
        ))
    })?;

    let const_value = ConstValue::from_json(field_value.clone())
        .map_err(|e| Error::IO(format!("Failed to convert to ConstValue: {e}")))?;

    Ok(const_value)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn test_parse_sse_graphql_event_success() {
        let data = r#"{"data":{"newMessage":{"id":"1","text":"hello"}}}"#;
        let result = parse_sse_graphql_event(data, "newMessage");
        assert!(result.is_ok());
        let value = result.unwrap();
        assert_eq!(
            value,
            ConstValue::from_json(serde_json::json!({"id": "1", "text": "hello"})).unwrap()
        );
    }

    #[test]
    fn test_parse_sse_graphql_event_with_errors() {
        let data = r#"{"errors":[{"message":"something went wrong"}]}"#;
        let result = parse_sse_graphql_event(data, "newMessage");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("something went wrong"));
    }

    #[test]
    fn test_parse_sse_graphql_event_missing_data() {
        let data = r#"{"extensions":{}}"#;
        let result = parse_sse_graphql_event(data, "newMessage");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_sse_graphql_event_missing_field() {
        let data = r#"{"data":{"otherField":"value"}}"#;
        let result = parse_sse_graphql_event(data, "newMessage");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_sse_graphql_event_invalid_json() {
        let result = parse_sse_graphql_event("not json", "newMessage");
        assert!(result.is_err());
    }
}
