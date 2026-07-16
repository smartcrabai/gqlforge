use anyhow::{Context, Result};

use crate::core::postgres::schema::{Column, DatabaseSchema, PgType, PrimaryKey, Table};

/// Connect to a `GreptimeDB` instance over its `PostgreSQL`-compatible protocol
/// and collect the subset of schema metadata it exposes through
/// `information_schema`.
///
/// `GreptimeDB` does not expose `PostgreSQL`'s `udt_name`, `is_generated`, or
/// `pg_catalog` catalogs, so it must not use the `PostgreSQL` introspector.
///
/// # Errors
///
/// Returns an error if the connection or schema queries fail.
pub async fn introspect(connection_url: &str) -> Result<DatabaseSchema> {
    let tls = crate::core::postgres::make_tls_connect()?;
    let (client, connection) = tokio_postgres::connect(connection_url, tls)
        .await
        .with_context(|| "Failed to connect to GreptimeDB".to_string())?;

    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!("GreptimeDB connection error: {error}");
        }
    });

    let tables_query = r"
        SELECT table_schema, table_name, table_type
        FROM information_schema.tables
        WHERE table_schema <> 'information_schema'
          AND table_type IN ('BASE TABLE', 'VIEW')
        ORDER BY table_schema, table_name
    ";
    let table_rows = client.query(tables_query, &[]).await?;
    let mut schema = DatabaseSchema::new();

    for row in table_rows {
        let table_schema: String = row.get("table_schema");
        let table_name: String = row.get("table_name");
        let table_type: String = row.get("table_type");
        let (columns, primary_key) = fetch_columns(&client, &table_schema, &table_name).await?;

        schema.add_table(Table {
            schema: table_schema,
            name: table_name,
            columns,
            primary_key,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: table_type == "VIEW",
        });
    }

    Ok(schema)
}

async fn fetch_columns(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<(Vec<Column>, Option<PrimaryKey>)> {
    let query = r"
        SELECT
            column_name,
            greptime_data_type,
            data_type,
            is_nullable,
            column_default,
            generation_expression,
            column_key
        FROM information_schema.columns
        WHERE table_schema = $1 AND table_name = $2
        ORDER BY ordinal_position
    ";
    let rows = client.query(query, &[&schema, &table]).await?;
    let mut columns = Vec::with_capacity(rows.len());
    let mut primary_key_columns = Vec::new();

    for row in rows {
        let name: String = row.get("column_name");
        if row.get::<_, String>("column_key") == "PRI" {
            primary_key_columns.push(name.clone());
        }
        columns.push(Column {
            name,
            pg_type: greptime_type_to_pg_type(
                &row.get::<_, String>("greptime_data_type"),
                &row.get::<_, String>("data_type"),
            ),
            is_nullable: row.get::<_, String>("is_nullable") == "YES",
            has_default: row.get::<_, Option<String>>("column_default").is_some(),
            is_generated: !row.get::<_, String>("generation_expression").is_empty(),
        });
    }

    Ok((
        columns,
        (!primary_key_columns.is_empty()).then_some(PrimaryKey { columns: primary_key_columns }),
    ))
}

fn greptime_type_to_pg_type(greptime_data_type: &str, data_type: &str) -> PgType {
    let greptime_data_type = greptime_data_type.to_ascii_lowercase();
    match greptime_data_type.as_str() {
        "int8" | "int16" | "uint8" | "uint16" => PgType::SmallInt,
        "int32" => PgType::Integer,
        "uint32" | "int64" | "uint64" => PgType::BigInt,
        "float32" => PgType::Real,
        "float64" => PgType::DoublePrecision,
        "string" => PgType::Text,
        "binary" => PgType::Bytea,
        "boolean" => PgType::Boolean,
        "date" => PgType::Date,
        "timestampsecond"
        | "timestampmillisecond"
        | "timestampmicrosecond"
        | "timestampnanosecond" => PgType::Timestamp,
        "interval" => PgType::Interval,
        "json" => PgType::Json,
        _ if greptime_data_type.starts_with("decimal") => PgType::Numeric,
        _ => PgType::from_sql_name(data_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_greptime_types_to_graphql_compatible_postgres_types() {
        assert_eq!(greptime_type_to_pg_type("String", "string"), PgType::Text);
        assert_eq!(
            greptime_type_to_pg_type("Float64", "double"),
            PgType::DoublePrecision
        );
        assert_eq!(
            greptime_type_to_pg_type("TimestampMillisecond", "timestamp(3)"),
            PgType::Timestamp
        );
        assert_eq!(greptime_type_to_pg_type("UInt32", "uint"), PgType::BigInt);
        assert_eq!(
            greptime_type_to_pg_type("Decimal128(10, 2)", "decimal(10,2)"),
            PgType::Numeric
        );
    }
}
