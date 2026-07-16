use gqlforge_macros::{DirectiveDefinition, InputDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::PostgresOperation;
use crate::core::is_default;

/// The operation type supported by `@greptimedb`.
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
pub enum GreptimedbOperation {
    /// SELECT multiple rows.
    #[default]
    Select,
    /// SELECT one row.
    SelectOne,
    /// INSERT a row and return its affected row count.
    Insert,
    /// DELETE rows and return their affected row count.
    Delete,
}

impl From<GreptimedbOperation> for PostgresOperation {
    fn from(operation: GreptimedbOperation) -> Self {
        match operation {
            GreptimedbOperation::Select => Self::Select,
            GreptimedbOperation::SelectOne => Self::SelectOne,
            GreptimedbOperation::Insert => Self::Insert,
            GreptimedbOperation::Delete => Self::Delete,
        }
    }
}

/// The `@greptimedb` directive maps a GraphQL field to a `GreptimeDB` table
/// operation over its PostgreSQL-compatible protocol.
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
#[directive_definition(repeatable, lowercase_name, locations = "FieldDefinition")]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Greptimedb {
    /// The `@link(type: GreptimeDB)` id to use. Optional when only one
    /// PostgreSQL-compatible connection is defined.
    #[serde(default, skip_serializing_if = "is_default")]
    pub db: Option<String>,

    /// The target table name (optionally schema-qualified, e.g.
    /// "public.metrics").
    pub table: String,

    /// The operation to perform.
    #[serde(default, skip_serializing_if = "is_default")]
    pub operation: GreptimedbOperation,

    /// A JSON object describing the WHERE clause. Supports Mustache templates
    /// for dynamic values, e.g. `{"host": "{{.args.host}}"}`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub filter: Option<Value>,

    /// For INSERT: the input data source. Typically a Mustache template
    /// referencing the `input` argument, e.g. `"{{.args.input}}"`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub input: Option<String>,

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
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;
    use crate::core::directive::DirectiveCodec;

    #[test]
    fn deserializes_and_serializes_the_lowercase_directive_name() {
        let json = serde_json::json!({
            "table": "metrics",
            "operation": "SELECT_ONE",
            "filter": {"host": "{{.args.host}}"}
        });
        let directive: Greptimedb = serde_json::from_value(json).unwrap();

        assert_eq!(directive.operation, GreptimedbOperation::SelectOne);
        assert_eq!(Greptimedb::directive_name(), "greptimedb");
        assert_eq!(directive.to_directive().name.node.as_str(), "greptimedb");
    }

    #[test]
    fn rejects_postgres_only_operations() {
        for operation in ["UPDATE", "LISTEN"] {
            let result: Result<Greptimedb, _> = serde_json::from_value(serde_json::json!({
                "table": "metrics",
                "operation": operation
            }));
            assert!(
                result.is_err(),
                "{operation} must not be accepted by @greptimedb"
            );
        }
    }

    #[test]
    fn rejects_unsupported_batch_key() {
        let result: Result<Greptimedb, _> = serde_json::from_value(serde_json::json!({
            "table": "metrics",
            "batchKey": ["host"]
        }));
        assert!(result.is_err());
    }
}
