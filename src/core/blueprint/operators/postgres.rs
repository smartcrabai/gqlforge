use gqlforge_valid::{Valid, Validator};

use crate::core::blueprint::BlueprintError;
use crate::core::config::group_by::GroupBy;
use crate::core::config::{
    ConfigModule, GraphQLOperationType, Greptimedb, LinkType, Postgres, PostgresOperation,
    PostgresPayloadType,
};
use crate::core::ir::model::{IO, IR};
use crate::core::mustache::Mustache;
use crate::core::postgres::request_template::{RequestTemplate, ResultMode};

pub trait DatabaseDirective {
    fn db(&self) -> Option<&str>;
    fn table(&self) -> &str;
    fn operation(&self) -> PostgresOperation;
    fn filter(&self) -> Option<&serde_json::Value>;
    fn input(&self) -> Option<&str>;
    fn batch_key(&self) -> &[String];
    fn dedupe(&self) -> Option<bool>;
    fn limit(&self) -> Option<&str>;
    fn offset(&self) -> Option<&str>;
    fn order_by(&self) -> Option<&str>;
    fn directive_name(&self) -> &'static str;
    fn result_mode(&self) -> ResultMode;
    fn matches_connection(&self, link_type: &LinkType) -> bool;
    fn listen_config(&self) -> Option<(&str, &PostgresPayloadType)>;
}

impl DatabaseDirective for Postgres {
    fn db(&self) -> Option<&str> {
        self.db.as_deref()
    }

    fn table(&self) -> &str {
        &self.table
    }

    fn operation(&self) -> PostgresOperation {
        self.operation.clone()
    }

    fn filter(&self) -> Option<&serde_json::Value> {
        self.filter.as_ref()
    }

    fn input(&self) -> Option<&str> {
        self.input.as_deref()
    }

    fn batch_key(&self) -> &[String] {
        &self.batch_key
    }

    fn dedupe(&self) -> Option<bool> {
        self.dedupe
    }

    fn limit(&self) -> Option<&str> {
        self.limit.as_deref()
    }

    fn offset(&self) -> Option<&str> {
        self.offset.as_deref()
    }

    fn order_by(&self) -> Option<&str> {
        self.order_by.as_deref()
    }

    fn directive_name(&self) -> &'static str {
        "@postgres"
    }

    fn result_mode(&self) -> ResultMode {
        ResultMode::Rows
    }

    fn matches_connection(&self, link_type: &LinkType) -> bool {
        matches!(link_type, LinkType::Postgres | LinkType::AuroraDsql)
    }

    fn listen_config(&self) -> Option<(&str, &PostgresPayloadType)> {
        self.channel
            .as_deref()
            .map(|channel| (channel, &self.payload_type))
    }
}

impl DatabaseDirective for Greptimedb {
    fn db(&self) -> Option<&str> {
        self.db.as_deref()
    }

    fn table(&self) -> &str {
        &self.table
    }

    fn operation(&self) -> PostgresOperation {
        self.operation.clone().into()
    }

    fn filter(&self) -> Option<&serde_json::Value> {
        self.filter.as_ref()
    }

    fn input(&self) -> Option<&str> {
        self.input.as_deref()
    }

    fn batch_key(&self) -> &[String] {
        &[]
    }

    fn dedupe(&self) -> Option<bool> {
        self.dedupe
    }

    fn limit(&self) -> Option<&str> {
        self.limit.as_deref()
    }

    fn offset(&self) -> Option<&str> {
        self.offset.as_deref()
    }

    fn order_by(&self) -> Option<&str> {
        self.order_by.as_deref()
    }

    fn directive_name(&self) -> &'static str {
        "@greptimedb"
    }

    fn result_mode(&self) -> ResultMode {
        match self.operation {
            crate::core::config::GreptimedbOperation::Insert
            | crate::core::config::GreptimedbOperation::Delete => ResultMode::AffectedRows,
            crate::core::config::GreptimedbOperation::Select
            | crate::core::config::GreptimedbOperation::SelectOne => ResultMode::Rows,
        }
    }

    fn matches_connection(&self, link_type: &LinkType) -> bool {
        matches!(link_type, LinkType::GreptimeDb)
    }

    fn listen_config(&self) -> Option<(&str, &PostgresPayloadType)> {
        None
    }
}

pub struct CompilePostgres<'a, D: DatabaseDirective> {
    pub config_module: &'a ConfigModule,
    pub postgres: &'a D,
    pub operation_type: &'a GraphQLOperationType,
}
impl<D: DatabaseDirective> Copy for CompilePostgres<'_, D> {}

impl<D: DatabaseDirective> Clone for CompilePostgres<'_, D> {
    fn clone(&self) -> Self {
        *self
    }
}

#[must_use]
#[expect(clippy::too_many_lines)]
pub fn compile_postgres<D: DatabaseDirective>(
    inputs: CompilePostgres<D>,
) -> Valid<IR, BlueprintError> {
    let pg = inputs.postgres;
    let operation_type = inputs.operation_type;
    let dedupe = pg.dedupe().unwrap_or_default();
    let schemas = &inputs.config_module.extensions().database_schemas;

    // Resolve the connection id. When database links are configured, the
    // directive may only select a connection of its own database kind.
    let matching_links: Vec<_> = inputs
        .config_module
        .links
        .iter()
        .filter(|link| pg.matches_connection(&link.type_of))
        .collect();
    let has_database_links = inputs.config_module.links.iter().any(|link| {
        matches!(
            link.type_of,
            LinkType::Postgres | LinkType::GreptimeDb | LinkType::AuroraDsql
        )
    });
    let connection_id = match pg.db() {
        Some(id) => match inputs.config_module.links.iter().find(|link| {
            link.id.as_deref().unwrap_or("default") == id
                && matches!(
                    link.type_of,
                    LinkType::Postgres | LinkType::GreptimeDb | LinkType::AuroraDsql
                )
        }) {
            Some(link) if !pg.matches_connection(&link.type_of) => {
                return Valid::fail(BlueprintError::Cause(format!(
                    "{} cannot use @link(type: {}) with db: '{id}'",
                    pg.directive_name(),
                    link.type_of
                )));
            }
            None if has_database_links => {
                return Valid::fail(BlueprintError::Cause(format!(
                    "{} references unknown database connection '{id}'",
                    pg.directive_name()
                )));
            }
            Some(_) | None => id.to_string(),
        },
        None if matching_links.len() == 1 => matching_links[0]
            .id
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        None if matching_links.len() > 1 => {
            return Valid::fail(BlueprintError::Cause(format!(
                "{} requires 'db' when multiple matching connections are defined",
                pg.directive_name()
            )));
        }
        None if has_database_links => {
            return Valid::fail(BlueprintError::Cause(format!(
                "{} requires a matching database link",
                pg.directive_name()
            )));
        }
        None if schemas.len() == 1 => schemas[0]
            .id
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        None if schemas.is_empty() => "default".to_string(),
        None => {
            return Valid::fail(BlueprintError::Cause(format!(
                "{} requires 'db' when multiple database schemas are defined",
                pg.directive_name()
            )));
        }
    };

    // LISTEN handling -- only PostgreSQL supports subscriptions.
    if matches!(pg.operation(), PostgresOperation::Listen) {
        let Some((channel, payload_type)) = pg.listen_config() else {
            return Valid::fail(BlueprintError::Cause(format!(
                "{}(operation: LISTEN) requires a non-empty 'channel'",
                pg.directive_name()
            )));
        };
        let is_subscription = matches!(operation_type, GraphQLOperationType::Subscription);
        if !is_subscription {
            return Valid::fail(BlueprintError::Cause(format!(
                "{}(operation: LISTEN) is only allowed on Subscription fields",
                pg.directive_name()
            )));
        }
        if channel.is_empty() {
            return Valid::fail(BlueprintError::Cause(format!(
                "{}(operation: LISTEN) requires a non-empty 'channel'",
                pg.directive_name()
            )));
        }
        if !channel.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Valid::fail(BlueprintError::Cause(format!(
                "Invalid channel name '{channel}': only alphanumeric and underscore allowed"
            )));
        }

        return Valid::succeed(IR::IO(Box::new(IO::PostgresStream {
            connection_id,
            channel: channel.to_string(),
            payload_type: payload_type.clone(),
        })));
    }

    if matches!(operation_type, GraphQLOperationType::Subscription) {
        return Valid::fail(BlueprintError::Cause(format!(
            "{} on Subscription requires operation: LISTEN",
            pg.directive_name()
        )));
    }

    let db_schema = inputs
        .config_module
        .extensions()
        .find_database_schema(Some(&connection_id));
    let resolved_table = db_schema.and_then(|schema| schema.find_table(pg.table()));

    let table_valid = if let Some(table) = resolved_table {
        if table.is_view
            && matches!(
                pg.operation(),
                PostgresOperation::Insert | PostgresOperation::Update | PostgresOperation::Delete
            )
        {
            Valid::fail(BlueprintError::Cause(format!(
                "Cannot perform {} on view '{}'. Standard views do not support write operations; use a base table.",
                pg.operation(),
                pg.table()
            )))
        } else {
            Valid::succeed(())
        }
    } else if db_schema.is_some() {
        Valid::fail(BlueprintError::Cause(format!(
            "Table '{}' not found in database schema",
            pg.table()
        )))
    } else {
        Valid::succeed(())
    };

    table_valid.map(|()| {
        let filter = pg.filter().cloned();
        let input = pg.input().map(Mustache::parse);
        let limit = pg.limit().map(Mustache::parse);
        let offset = pg.offset().map(Mustache::parse);
        let order_by = pg.order_by().map(Mustache::parse);
        let columns = resolved_table
            .map(|table| {
                table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        let req_template = RequestTemplate {
            table: resolved_table.map_or_else(
                || pg.table().to_string(),
                crate::core::postgres::schema::Table::qualified_name,
            ),
            operation: pg.operation(),
            filter,
            input,
            limit,
            offset,
            order_by,
            columns,
            result_mode: pg.result_mode(),
        };

        let io = IO::Postgres {
            req_template,
            group_by: (!pg.batch_key().is_empty())
                .then(|| GroupBy::new(pg.batch_key().to_vec(), None)),
            dl_id: None,
            dedupe,
            connection_id,
        };
        IR::IO(Box::new(io))
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use gqlforge_valid::Validator;

    use super::*;
    use crate::core::config::{Config, Content, Extensions, GreptimedbOperation};
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
    fn unqualified_table_uses_its_resolved_schema() {
        let mut schema = DatabaseSchema::new();
        let mut table = make_table("events");
        table.schema = "metrics".to_string();
        schema.add_table(table);
        let cm = make_config_module(vec![Content {
            id: Some("main".to_string()),
            content: schema,
        }]);
        let pg = Postgres {
            db: Some("main".to_string()),
            table: "events".to_string(),
            ..Default::default()
        };

        let ir = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &pg,
            operation_type: &GraphQLOperationType::Query,
        })
        .to_result()
        .unwrap();

        assert!(matches!(
            ir,
            IR::IO(io) if matches!(
                io.as_ref(),
                IO::Postgres { req_template, .. } if req_template.table == "metrics.events"
            )
        ));
    }

    fn greptime_config_module() -> ConfigModule {
        let mut config = Config::default();
        config.links.push(crate::core::config::Link {
            id: Some("metrics".to_string()),
            src: "postgresql://greptime@localhost:4003/public".to_string(),
            type_of: LinkType::GreptimeDb,
            ..Default::default()
        });
        let mut extensions = Extensions::default();
        extensions.add_database_schema(Some("metrics".to_string()), make_schema("users"));
        ConfigModule::new(config, extensions)
    }

    #[test]
    fn greptimedb_insert_returns_affected_rows() {
        let cm = greptime_config_module();
        let db = Greptimedb {
            db: Some("metrics".to_string()),
            table: "users".to_string(),
            operation: GreptimedbOperation::Insert,
            ..Default::default()
        };

        let ir = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &db,
            operation_type: &GraphQLOperationType::Mutation,
        })
        .to_result()
        .unwrap();

        match ir {
            IR::IO(io) => match io.as_ref() {
                IO::Postgres { req_template, .. } => {
                    assert_eq!(req_template.result_mode, ResultMode::AffectedRows);
                }
                other => panic!("Expected IO::Postgres, got: {other:?}"),
            },
            other => panic!("Expected IR::IO, got: {other:?}"),
        }
    }

    #[test]
    fn greptimedb_select_one_uses_single_row_operation() {
        let cm = greptime_config_module();
        let db = Greptimedb {
            db: None,
            table: "users".to_string(),
            operation: GreptimedbOperation::SelectOne,
            ..Default::default()
        };

        let ir = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &db,
            operation_type: &GraphQLOperationType::Query,
        })
        .to_result()
        .unwrap();

        assert!(matches!(
            ir,
            IR::IO(io) if matches!(
                io.as_ref(),
                IO::Postgres {
                    connection_id,
                    req_template,
                    ..
                } if connection_id == "metrics"
                    && req_template.operation == PostgresOperation::SelectOne
                    && req_template.result_mode == ResultMode::Rows
            )
        ));
    }

    #[test]
    fn greptimedb_rejects_unknown_connection() {
        let cm = greptime_config_module();
        let db = Greptimedb {
            db: Some("typo".to_string()),
            table: "users".to_string(),
            ..Default::default()
        };

        let error = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &db,
            operation_type: &GraphQLOperationType::Query,
        })
        .to_result()
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("@greptimedb references unknown database connection 'typo'")
        );
    }

    #[test]
    fn greptimedb_rejects_postgres_connection() {
        let mut config = Config::default();
        config.links.push(crate::core::config::Link {
            id: Some("primary".to_string()),
            src: "postgresql://postgres@localhost/app".to_string(),
            type_of: LinkType::Postgres,
            ..Default::default()
        });
        let cm = ConfigModule::new(config, Extensions::default());
        let db = Greptimedb {
            db: Some("primary".to_string()),
            table: "metrics".to_string(),
            ..Default::default()
        };

        let error = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &db,
            operation_type: &GraphQLOperationType::Query,
        })
        .to_result()
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("@greptimedb cannot use @link(type: Postgres)"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn postgres_rejects_greptimedb_connection() {
        let cm = greptime_config_module();
        let db = Postgres {
            db: Some("metrics".to_string()),
            table: "users".to_string(),
            ..Default::default()
        };

        let error = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &db,
            operation_type: &GraphQLOperationType::Query,
        })
        .to_result()
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("@postgres cannot use @link(type: GreptimeDB)"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn greptimedb_delete_returns_affected_rows() {
        let cm = greptime_config_module();
        let delete = Greptimedb {
            db: Some("metrics".to_string()),
            table: "users".to_string(),
            operation: GreptimedbOperation::Delete,
            filter: Some(serde_json::json!({"id": "{{.args.id}}"})),
            ..Default::default()
        };
        let ir = compile_postgres(CompilePostgres {
            config_module: &cm,
            postgres: &delete,
            operation_type: &GraphQLOperationType::Mutation,
        })
        .to_result()
        .unwrap();
        assert!(matches!(
            ir,
            IR::IO(io) if matches!(
                io.as_ref(),
                IO::Postgres { req_template, .. } if req_template.result_mode == ResultMode::AffectedRows
            )
        ));
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
        // No schema -> skips table validation, succeeds with connection_id
        // "default"
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

    // Tests for @postgres(operation: LISTEN) -- PostgresStream

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
