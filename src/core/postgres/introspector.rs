use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::join_all;

use super::schema::{
    Column, DatabaseSchema, ForeignKey, PgType, PrimaryKey, Table, UniqueConstraint,
};

fn redact_url(url: &str) -> String {
    if let Some(at) = url.find('@')
        && let Some(scheme_end) = url.find("://")
    {
        return format!("{}://***{}", &url[..scheme_end], &url[at..]);
    }
    "<redacted>".to_string()
}

/// Connect to a live `PostgreSQL` instance and introspect its schema.
///
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn introspect(connection_url: &str) -> Result<DatabaseSchema> {
    let tls = super::make_tls_connect()?;
    let (client, connection) = tokio_postgres::connect(connection_url, tls)
        .await
        .with_context(|| {
            format!(
                "Failed to connect to PostgreSQL: {}",
                redact_url(connection_url)
            )
        })?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("PostgreSQL connection error: {e}");
        }
    });

    let client = Arc::new(client);
    let mut schema = DatabaseSchema::new();

    // Fetch tables, views, and their metadata (columns, primary keys, foreign keys,
    // unique constraints) in parallel per table.
    let tables_query = r"
        SELECT table_schema, table_name, table_type
        FROM information_schema.tables
        WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
          AND table_type IN ('BASE TABLE', 'VIEW')
        ORDER BY table_schema, table_name
    ";
    let table_rows = client.query(tables_query, &[]).await?;

    for row in &table_rows {
        let table_schema: String = row.get("table_schema");
        let table_name: String = row.get("table_name");
        let table_type: String = row.get("table_type");
        let is_view = table_type == "VIEW";

        let columns = fetch_columns(&client, &table_schema, &table_name);
        let primary_key = fetch_primary_key(&client, &table_schema, &table_name);
        let foreign_keys = fetch_foreign_keys(&client, &table_schema, &table_name);
        let unique_constraints = fetch_unique_constraints(&client, &table_schema, &table_name);
        let (columns, primary_key, foreign_keys, unique_constraints) =
            tokio::join!(columns, primary_key, foreign_keys, unique_constraints);
        let columns = columns?;
        let primary_key = primary_key?;
        let foreign_keys = foreign_keys?;
        let unique_constraints = unique_constraints?;

        schema.add_table(Table {
            schema: table_schema,
            name: table_name,
            columns,
            primary_key,
            foreign_keys,
            unique_constraints,
            is_view,
        });
    }

    // Materialized views use pg_catalog.pg_matviews since information_schema omits
    // them (relkind = 'm').
    let matviews_query = r"
        SELECT schemaname AS table_schema, matviewname AS table_name
        FROM pg_catalog.pg_matviews
        WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
        ORDER BY schemaname, matviewname
    ";
    let matview_rows = client.query(matviews_query, &[]).await.or_else(|e| {
        if e.as_db_error().is_some_and(|db_err| {
            db_err.code() == &tokio_postgres::error::SqlState::UNDEFINED_TABLE
        }) {
            tracing::warn!(
                "Materialized views not available (pg_catalog.pg_matviews not found): {e}"
            );
            Ok(vec![])
        } else {
            Err(anyhow::Error::new(e).context("Failed to query materialized views"))
        }
    })?;
    let matview_futures: Vec<_> = matview_rows
        .iter()
        .map(|row| {
            let client = Arc::clone(&client);
            async move {
                let table_schema: String = row.get("table_schema");
                let table_name: String = row.get("table_name");
                let columns = fetch_matview_columns(&client, &table_schema, &table_name).await?;
                anyhow::Result::<_>::Ok((table_schema, table_name, columns))
            }
        })
        .collect();
    let matview_results = join_all(matview_futures).await;

    for result in matview_results {
        let (table_schema, table_name, columns) = result?;
        schema.add_table(Table {
            schema: table_schema,
            name: table_name,
            columns,
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: true,
        });
    }

    Ok(schema)
}

/// Fetch columns for a materialized view using `pg_catalog.pg_attribute`.
/// `information_schema.columns` excludes materialized views (relkind = 'm'),
/// so we query the system catalog directly.
async fn fetch_matview_columns(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<Vec<Column>> {
    let query = r"
        SELECT
            a.attname AS column_name,
            pg_catalog.format_type(a.atttypid, a.atttypmod) AS data_type,
            (NOT a.attnotnull) AS is_nullable
        FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid
        WHERE n.nspname = $1
          AND c.relname = $2
          AND c.relkind = 'm'
          AND a.attnum > 0
          AND NOT a.attisdropped
        ORDER BY a.attnum
    ";
    let rows = client.query(query, &[&schema, &table]).await?;

    let mut columns = Vec::new();
    for row in rows {
        let name: String = row.get("column_name");
        let data_type: String = row.get("data_type");
        let is_nullable: bool = row.get("is_nullable");

        columns.push(Column {
            name,
            pg_type: PgType::from_sql_name(&data_type),
            is_nullable,
            has_default: false,
            is_generated: false,
        });
    }

    Ok(columns)
}

async fn fetch_columns(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<Vec<Column>> {
    let query = r"
        SELECT
            column_name,
            data_type,
            udt_name,
            is_nullable,
            column_default,
            is_generated
        FROM information_schema.columns
        WHERE table_schema = $1 AND table_name = $2
        ORDER BY ordinal_position
    ";
    let rows = client.query(query, &[&schema, &table]).await?;

    let mut columns = Vec::new();
    for row in rows {
        let name: String = row.get("column_name");
        let data_type: String = row.get("data_type");
        let udt_name: String = row.get("udt_name");
        let is_nullable: String = row.get("is_nullable");
        let column_default: Option<String> = row.get("column_default");
        let is_generated: String = row.get("is_generated");

        let type_name = if data_type == "USER-DEFINED" {
            udt_name
        } else {
            data_type
        };

        columns.push(Column {
            name,
            pg_type: PgType::from_sql_name(&type_name),
            is_nullable: is_nullable == "YES",
            has_default: column_default.is_some(),
            is_generated: is_generated != "NEVER",
        });
    }

    Ok(columns)
}

async fn fetch_primary_key(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<Option<PrimaryKey>> {
    let query = r"
        SELECT kcu.column_name
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON tc.constraint_name = kcu.constraint_name
         AND tc.table_schema = kcu.table_schema
        WHERE tc.constraint_type = 'PRIMARY KEY'
          AND tc.table_schema = $1
          AND tc.table_name = $2
        ORDER BY kcu.ordinal_position
    ";
    let rows = client.query(query, &[&schema, &table]).await?;

    if rows.is_empty() {
        return Ok(None);
    }

    let columns: Vec<String> = rows.iter().map(|r| r.get("column_name")).collect();
    Ok(Some(PrimaryKey { columns }))
}

async fn fetch_foreign_keys(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<Vec<ForeignKey>> {
    let query = r"
        SELECT
            fk_kcu.column_name          AS column_name,
            ref_tc.table_schema         AS foreign_table_schema,
            ref_tc.table_name           AS foreign_table_name,
            ref_kcu.column_name         AS foreign_column_name,
            rc.constraint_name
        FROM information_schema.referential_constraints rc
        JOIN information_schema.table_constraints fk_tc
          ON fk_tc.constraint_name = rc.constraint_name
         AND fk_tc.table_schema    = rc.constraint_schema
        JOIN information_schema.key_column_usage fk_kcu
          ON fk_kcu.constraint_name = rc.constraint_name
         AND fk_kcu.table_schema    = rc.constraint_schema
        JOIN information_schema.table_constraints ref_tc
          ON ref_tc.constraint_name = rc.unique_constraint_name
         AND ref_tc.table_schema    = rc.unique_constraint_schema
        JOIN information_schema.key_column_usage ref_kcu
          ON ref_kcu.constraint_name    = rc.unique_constraint_name
         AND ref_kcu.table_schema       = rc.unique_constraint_schema
         AND ref_kcu.ordinal_position   = fk_kcu.position_in_unique_constraint
        WHERE fk_tc.table_schema = $1
          AND fk_tc.table_name   = $2
        ORDER BY rc.constraint_name, fk_kcu.ordinal_position
    ";
    let rows = client.query(query, &[&schema, &table]).await?;

    // Group by constraint name.
    let mut fk_map: std::collections::BTreeMap<String, ForeignKey> =
        std::collections::BTreeMap::new();
    for row in rows {
        let constraint: String = row.get("constraint_name");
        let col: String = row.get("column_name");
        let ref_schema: String = row.get("foreign_table_schema");
        let ref_table: String = row.get("foreign_table_name");
        let ref_col: String = row.get("foreign_column_name");

        let entry = fk_map.entry(constraint).or_insert_with(|| ForeignKey {
            columns: Vec::new(),
            referenced_schema: ref_schema,
            referenced_table: ref_table,
            referenced_columns: Vec::new(),
        });
        entry.columns.push(col);
        entry.referenced_columns.push(ref_col);
    }

    Ok(fk_map.into_values().collect())
}

async fn fetch_unique_constraints(
    client: &tokio_postgres::Client,
    schema: &str,
    table: &str,
) -> Result<Vec<UniqueConstraint>> {
    let query = r"
        SELECT kcu.column_name, tc.constraint_name
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON tc.constraint_name = kcu.constraint_name
         AND tc.table_schema = kcu.table_schema
        WHERE tc.constraint_type = 'UNIQUE'
          AND tc.table_schema = $1
          AND tc.table_name = $2
        ORDER BY tc.constraint_name, kcu.ordinal_position
    ";
    let rows = client.query(query, &[&schema, &table]).await?;

    let mut uc_map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for row in rows {
        let constraint: String = row.get("constraint_name");
        let col: String = row.get("column_name");
        uc_map.entry(constraint).or_default().push(col);
    }

    Ok(uc_map
        .into_values()
        .map(|columns| UniqueConstraint { columns })
        .collect())
}
