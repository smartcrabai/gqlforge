use gqlforge_valid::{Valid, Validator};

use crate::core::blueprint::BlueprintError;
use crate::core::config::group_by::GroupBy;
use crate::core::config::{ConfigModule, GraphQLOperationType, Postgres, PostgresOperation};
use crate::core::ir::model::{IO, IR};
use crate::core::mustache::Mustache;
use crate::core::postgres::request_template::RequestTemplate;

#[derive(Clone, Copy)]
pub struct CompilePostgres<'a> {
    pub config_module: &'a ConfigModule,
    pub postgres: &'a Postgres,
    pub operation_type: &'a GraphQLOperationType,
}

#[must_use]
#[expect(clippy::too_many_lines)]
pub fn compile_postgres(inputs: CompilePostgres) -> Valid<IR, BlueprintError> {
    let pg = inputs.postgres;
    let operation_type = inputs.operation_type;
    let dedupe = pg.dedupe.unwrap_or_default();
    let schemas = &inputs.config_module.extensions().database_schemas;

    // Resolve the connection id.
    let connection_id = match &pg.db {
        Some(id) => id.clone(),
        None => {
            if schemas.len() == 1 {
                schemas[0]
                    .id
                    .clone()
                    .unwrap_or_else(|| "default".to_string())
            } else if schemas.is_empty() {
                "default".to_string()
            } else {
                return Valid::fail(BlueprintError::Cause(
                    "@postgres requires 'db' when multiple Postgres connections are defined"
                        .to_string(),
                ));
            }
        }
    };

    // LISTEN handling — only allowed on Subscription fields
    if pg.operation == PostgresOperation::Listen {
        let is_subscription = matches!(operation_type, GraphQLOperationType::Subscription);
        if !is_subscription {
            return Valid::fail(BlueprintError::Cause(
                "@postgres(operation: LISTEN) is only allowed on Subscription fields".to_string(),
            ));
        }
        let channel = match pg.channel.as_deref() {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => {
                return Valid::fail(BlueprintError::Cause(
                    "@postgres(operation: LISTEN) requires a non-empty 'channel'".to_string(),
                ));
            }
        };
        // Validate channel name
        if !channel.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Valid::fail(BlueprintError::Cause(format!(
                "Invalid channel name '{channel}': only alphanumeric and underscore allowed"
            )));
        }

        return Valid::succeed(IR::IO(Box::new(IO::PostgresStream {
            connection_id,
            channel,
            payload_type: pg.payload_type.clone(),
        })));
    }

    // Non-Listen operations on Subscription fields are not allowed
    if matches!(operation_type, GraphQLOperationType::Subscription) {
        return Valid::fail(BlueprintError::Cause(
            "@postgres on Subscription requires operation: LISTEN".to_string(),
        ));
    }

    // Validate that the table exists in the database schema (if available).
    let db_schema = inputs
        .config_module
        .extensions()
        .find_database_schema(Some(&connection_id));

    let table_valid = if let Some(schema) = db_schema {
        if let Some(table) = schema.find_table(&pg.table) {
            if table.is_view
                && matches!(
                    pg.operation,
                    PostgresOperation::Insert
                        | PostgresOperation::Update
                        | PostgresOperation::Delete
                )
            {
                Valid::fail(BlueprintError::Cause(format!(
                    "Cannot perform {} on view '{}'. \
                     Standard views do not support write operations; \
                     use a base table or define INSTEAD OF triggers on the view.",
                    pg.operation, pg.table
                )))
            } else {
                Valid::succeed(())
            }
        } else {
            Valid::fail(BlueprintError::Cause(format!(
                "Table '{}' not found in database schema",
                pg.table
            )))
        }
    } else {
        // If no database schema is loaded, skip validation (it will be
        // validated at runtime).
        Valid::succeed(())
    };

    table_valid.map(|()| {
        let filter = pg.filter.as_ref().map(|v| Mustache::parse(&v.to_string()));
        let input = pg.input.as_ref().map(|v| Mustache::parse(v));
        let limit = pg.limit.as_ref().map(|v| Mustache::parse(v));
        let offset = pg.offset.as_ref().map(|v| Mustache::parse(v));
        let order_by = pg.order_by.as_ref().map(|v| Mustache::parse(v));

        // Determine columns from database schema if available.
        let columns = db_schema
            .and_then(|s| s.find_table(&pg.table))
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();

        let req_template = RequestTemplate {
            table: pg.table.clone(),
            operation: pg.operation.clone(),
            filter,
            input,
            limit,
            offset,
            order_by,
            columns,
        };

        let io = if pg.batch_key.is_empty() {
            IO::Postgres {
                req_template,
                group_by: None,
                dl_id: None,
                dedupe,
                connection_id,
            }
        } else {
            IO::Postgres {
                req_template,
                group_by: Some(GroupBy::new(pg.batch_key.clone(), None)),
                dl_id: None,
                dedupe,
                connection_id,
            }
        };

        IR::IO(Box::new(io))
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use gqlforge_valid::Validator;

    use super::*;
    use crate::core::config::{Config, Content, Extensions};
    use crate::core::postgres::PostgresPayloadType;
    use crate::core::postgres::schema::{Column, DatabaseSchema, PgType, Table};

    fn make_table(name: &str) -> Table {
        Table {
            schema: "public".to_string(),
            name: name.to_string(),
            columns: vec![
                Column {
                    name: "id".to_string(),
                    pg_type: PgType::Integer,
                    is_nullable: false,
                    has_default: true,
                    is_generated: false,
                },
                Column {
                    name: "name".to_string(),
                    pg_type: PgType::Text,
                    is_nullable: false,
                    has_default: false,
                    is_generated: false,
                },
            ],
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: false,
        }
    }

    fn make_view_table(name: &str) -> Table {
        Table { is_view: true, ..make_table(name) }
    }

    fn make_view_schema(table_name: &str) -> DatabaseSchema {
        let mut schema = DatabaseSchema::new();
        schema.add_table(make_view_table(table_name));
        schema
    }

    fn make_schema(table_name: &str) -> DatabaseSchema {
        let mut schema = DatabaseSchema::new();
        schema.add_table(make_table(table_name));
        schema
    }

    fn make_config_module(schemas: Vec<Content<DatabaseSchema>>) -> ConfigModule {
        let mut ext = Extensions::default();
        for s in schemas {
            ext.add_database_schema(s.id, s.content);
        }
        ConfigModule::new(Config::default(), ext)
    }

    #[test]
    fn single_schema_no_db_succeeds() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres { table: "users".to_string(), ..Default::default() };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_ok());
    }

    #[test]
    fn no_schema_uses_default_id() {
        let cm = make_config_module(vec![]);
        let pg = Postgres { table: "users".to_string(), ..Default::default() };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        // No schema -> skips table validation, succeeds with connection_id "default"
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Postgres { connection_id, .. } => {
                    assert_eq!(connection_id, "default");
                }
                other => panic!("Expected IO::Postgres, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn multiple_schemas_no_db_fails() {
        let cm = make_config_module(vec![
            Content { id: Some("main".to_string()), content: make_schema("users") },
            Content {
                id: Some("analytics".to_string()),
                content: make_schema("events"),
            },
        ]);
        let pg = Postgres { table: "users".to_string(), ..Default::default() };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_err());
    }

    #[test]
    fn multiple_schemas_with_db_succeeds() {
        let cm = make_config_module(vec![
            Content { id: Some("main".to_string()), content: make_schema("users") },
            Content {
                id: Some("analytics".to_string()),
                content: make_schema("events"),
            },
        ]);
        let pg = Postgres {
            table: "users".to_string(),
            db: Some("main".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_ok());
    }

    #[test]
    fn nonexistent_table_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "nonexistent".to_string(),
            db: Some("main".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_err());
    }

    #[test]
    fn view_insert_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_view_schema("user_summary"),
        }]);
        let pg = Postgres {
            table: "user_summary".to_string(),
            operation: PostgresOperation::Insert,
            db: Some("main".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_err());
    }

    #[test]
    fn view_update_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_view_schema("user_summary"),
        }]);
        let pg = Postgres {
            table: "user_summary".to_string(),
            operation: PostgresOperation::Update,
            db: Some("main".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_err());
    }

    #[test]
    fn view_delete_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_view_schema("user_summary"),
        }]);
        let pg = Postgres {
            table: "user_summary".to_string(),
            operation: PostgresOperation::Delete,
            db: Some("main".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_err());
    }

    #[test]
    fn view_select_succeeds() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_view_schema("user_summary"),
        }]);
        let pg = Postgres {
            table: "user_summary".to_string(),
            operation: PostgresOperation::Select,
            db: Some("main".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        assert!(result.to_result().is_ok());
    }

    // Tests for @postgres(operation: LISTEN) — PostgresStream

    #[test]
    fn listen_subscription_succeeds_and_returns_postgres_stream() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: Some("users_changes".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::PostgresStream { connection_id, channel, payload_type } => {
                    assert_eq!(connection_id, "main");
                    assert_eq!(channel, "users_changes");
                    assert_eq!(*payload_type, PostgresPayloadType::Json);
                }
                other => panic!("Expected IO::PostgresStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn listen_on_query_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: Some("users_changes".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("LISTEN) is only allowed on Subscription"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn listen_on_mutation_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: Some("users_changes".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Mutation,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("LISTEN) is only allowed on Subscription"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn listen_without_channel_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: None,
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("LISTEN) requires a non-empty 'channel'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn listen_with_empty_channel_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: Some(String::new()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("LISTEN) requires a non-empty 'channel'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn listen_with_invalid_channel_name_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: Some("users; DROP TABLE".to_string()),
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string().contains("Invalid channel name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn select_on_subscription_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Select,
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("@postgres on Subscription requires operation: LISTEN"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn insert_on_subscription_fails() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Insert,
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let err = result.to_result().unwrap_err();
        assert!(
            err.to_string()
                .contains("@postgres on Subscription requires operation: LISTEN"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn listen_supports_custom_payload_type_text() {
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: make_schema("users"),
        }]);
        let pg = Postgres {
            table: "users".to_string(),
            operation: PostgresOperation::Listen,
            channel: Some("events".to_string()),
            payload_type: crate::core::postgres::PostgresPayloadType::Text,
            ..Default::default()
        };
        let result = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Subscription,
        });
        let ir = result.to_result().unwrap();
        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::PostgresStream { payload_type, .. } => {
                    assert_eq!(
                        *payload_type,
                        crate::core::postgres::PostgresPayloadType::Text
                    );
                }
                other => panic!("Expected IO::PostgresStream, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }
}
