use gqlforge_macros::{DirectiveDefinition, InputDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::is_default;

/// The operation type for a `@postgres` directive.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum_macros::Display,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PostgresOperation {
    /// SELECT multiple rows (returns a list).
    #[default]
    Select,
    /// SELECT a single row by primary key or unique constraint.
    SelectOne,
    /// INSERT a new row.
    Insert,
    /// UPDATE an existing row.
    Update,
    /// DELETE a row.
    Delete,
    /// LISTEN to a `PostgreSQL` channel and stream NOTIFY events.
    Listen,
}

/// How to interpret the NOTIFY payload.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum_macros::Display,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PostgresPayloadType {
    /// Parse the NOTIFY payload as JSON.
    #[default]
    Json,
    /// Treat the NOTIFY payload as a raw string, wrapped in
    /// `ConstValue::String`.
    Text,
}

/// The `@postgres` directive maps a GraphQL field to a `PostgreSQL` table
/// operation.
///
/// Supports CRUD operations with Mustache-templated filter expressions,
/// pagination, and batched data loading.
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    schemars::JsonSchema,
    InputDefinition,
    DirectiveDefinition,
)]
#[directive_definition(repeatable, locations = "FieldDefinition")]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Postgres {
    /// The `@link(type: Postgres)` id to use. Optional when only one Postgres
    /// link is defined.
    #[serde(default, skip_serializing_if = "is_default")]
    pub db: Option<String>,

    /// The target table name (optionally schema-qualified, e.g.
    /// "public.users").
    pub table: String,

    /// The CRUD operation to perform.
    #[serde(default, skip_serializing_if = "is_default")]
    pub operation: PostgresOperation,

    /// A JSON object describing the WHERE clause. Supports Mustache templates
    /// for dynamic values, e.g. `{"id": "{{.args.id}}"}`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub filter: Option<Value>,

    /// For INSERT/UPDATE: the input data source. Typically a Mustache template
    /// referencing the `input` argument, e.g. `"{{.args.input}}"`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub input: Option<String>,

    /// Columns used for `DataLoader` batch keys (N+1 prevention).
    #[serde(rename = "batchKey", default, skip_serializing_if = "is_default")]
    pub batch_key: Vec<String>,

    /// Enables deduplication of identical IO operations.
    #[serde(default, skip_serializing_if = "is_default")]
    pub dedupe: Option<bool>,

    /// Mustache template for the LIMIT clause, e.g. `"{{.args.limit}}"`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub limit: Option<String>,

    /// Mustache template for the OFFSET clause, e.g. `"{{.args.offset}}"`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub offset: Option<String>,

    /// Mustache template for the ORDER BY clause, e.g. `"{{.args.orderBy}}"`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub order_by: Option<String>,

    /// The channel name to LISTEN on (required when `operation: LISTEN`).
    /// Mustache templates are NOT supported for the channel name.
    #[serde(default, skip_serializing_if = "is_default")]
    pub channel: Option<String>,

    /// How to interpret the NOTIFY payload. Defaults to `JSON`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub payload_type: PostgresPayloadType,
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn deserialize_listen_operation() {
        let json = serde_json::json!({
            "table": "users",
            "operation": "LISTEN",
            "channel": "users_changes"
        });
        let pg: Postgres = serde_json::from_value(json).unwrap();
        assert_eq!(pg.operation, PostgresOperation::Listen);
        assert_eq!(pg.channel.as_deref(), Some("users_changes"));
    }

    #[test]
    fn deserialize_listen_with_payload_type_json() {
        let json = serde_json::json!({
            "table": "users",
            "operation": "LISTEN",
            "channel": "events",
            "payloadType": "JSON"
        });
        let pg: Postgres = serde_json::from_value(json).unwrap();
        assert_eq!(pg.operation, PostgresOperation::Listen);
        assert_eq!(pg.payload_type, PostgresPayloadType::Json);
    }

    #[test]
    fn deserialize_listen_with_payload_type_text() {
        let json = serde_json::json!({
            "table": "users",
            "operation": "LISTEN",
            "channel": "events",
            "payloadType": "TEXT"
        });
        let pg: Postgres = serde_json::from_value(json).unwrap();
        assert_eq!(pg.operation, PostgresOperation::Listen);
        assert_eq!(pg.payload_type, PostgresPayloadType::Text);
    }

    #[test]
    fn deserialize_listen_defaults_payload_type_to_json() {
        let json = serde_json::json!({
            "table": "users",
            "operation": "LISTEN",
            "channel": "events"
        });
        let pg: Postgres = serde_json::from_value(json).unwrap();
        assert_eq!(pg.payload_type, PostgresPayloadType::Json);
    }

    #[test]
    fn deserialize_payload_type_json_lowercase() {
        let json = serde_json::json!({
            "table": "users",
            "operation": "LISTEN",
            "channel": "events",
            "payloadType": "json"
        });
        // SCREAMING_SNAKE_CASE is case-sensitive; lowercase should fail
        let result: Result<Postgres, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }
}
