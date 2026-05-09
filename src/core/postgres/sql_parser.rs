use anyhow::{Context, Result};
use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, ColumnOptionDef, DataType, Expr, ObjectName,
    SelectItem, SetExpr, Statement, TableConstraint, TableFactor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use super::schema::{
    Column, DatabaseSchema, ForeignKey, PgType, PrimaryKey, Table, UniqueConstraint,
};

/// Parse one or more SQL migration strings (in order) into a `DatabaseSchema`.
///
/// # Errors
///
/// Returns an error if the operation fails.
pub fn parse_migrations(sql_contents: &[String]) -> Result<DatabaseSchema> {
    let mut schema = DatabaseSchema::new();
    let dialect = PostgreSqlDialect {};

    for (idx, sql) in sql_contents.iter().enumerate() {
        let statements = Parser::parse_sql(&dialect, sql)
            .with_context(|| format!("Failed to parse migration #{idx}"))?;

        for stmt in statements {
            apply_statement(&mut schema, &stmt);
        }
    }

    Ok(schema)
}

#[expect(clippy::too_many_lines, reason = "handles all SQL statement variants")]
fn apply_statement(schema: &mut DatabaseSchema, stmt: &Statement) {
    match stmt {
        Statement::CreateTable(create) => {
            let (table_schema, table_name) = extract_schema_and_name(&create.name);
            let mut columns = Vec::new();
            let mut primary_key = None;
            let mut foreign_keys = Vec::new();
            let mut unique_constraints = Vec::new();

            for col_def in &create.columns {
                columns.push(column_from_def(col_def));
            }

            for constraint in &create.constraints {
                match constraint {
                    TableConstraint::PrimaryKey(pk) => {
                        primary_key = Some(PrimaryKey {
                            columns: pk
                                .columns
                                .iter()
                                .filter_map(|c| {
                                    if let Expr::Identifier(ident) = &c.column.expr {
                                        Some(ident.value.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect(),
                        });
                    }
                    TableConstraint::ForeignKey(fk) => {
                        let (ref_schema, ref_table) = extract_schema_and_name(&fk.foreign_table);
                        foreign_keys.push(ForeignKey {
                            columns: fk.columns.iter().map(|c| c.value.clone()).collect(),
                            referenced_schema: ref_schema,
                            referenced_table: ref_table,
                            referenced_columns: fk
                                .referred_columns
                                .iter()
                                .map(|c| c.value.clone())
                                .collect(),
                        });
                    }
                    TableConstraint::Unique(u) => {
                        unique_constraints.push(UniqueConstraint {
                            columns: u
                                .columns
                                .iter()
                                .filter_map(|c| {
                                    if let Expr::Identifier(ident) = &c.column.expr {
                                        Some(ident.value.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect(),
                        });
                    }
                    _ => {}
                }
            }

            // Extract inline PK from column options if no table-level PK.
            if primary_key.is_none() {
                let inline_pk_cols: Vec<String> = columns
                    .iter()
                    .zip(create.columns.iter())
                    .filter_map(|(col, def)| {
                        if def.options.iter().any(|opt| {
                            matches!(
                                opt,
                                ColumnOptionDef { option: ColumnOption::PrimaryKey(_), .. }
                            )
                        }) {
                            Some(col.name.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                if !inline_pk_cols.is_empty() {
                    primary_key = Some(PrimaryKey { columns: inline_pk_cols });
                }
            }

            // Table-level PK columns must be non-nullable
            if let Some(ref pk) = primary_key {
                for col in &mut columns {
                    if pk.columns.contains(&col.name) {
                        col.is_nullable = false;
                    }
                }
            }

            let table = Table {
                schema: table_schema,
                name: table_name,
                columns,
                primary_key,
                foreign_keys,
                unique_constraints,
                is_view: false,
            };
            schema.add_table(table);
        }
        Statement::AlterTable(alter_table) => {
            let name = &alter_table.name;
            let qualified = format_object_name(name);
            let key = if schema.tables.contains_key(&qualified) {
                Some(qualified)
            } else {
                let (_, tname) = extract_schema_and_name(name);
                let alt = format!("public.{tname}");
                if schema.tables.contains_key(&alt) {
                    Some(alt)
                } else {
                    None
                }
            };
            if let Some(table) = key.and_then(|k| schema.tables.get_mut(&k)) {
                for op in &alter_table.operations {
                    apply_alter_op(table, op);
                }
            }
        }
        Statement::CreateView(create) => {
            let (view_schema, view_name) = extract_schema_and_name(&create.name);

            // Collect FROM-clause table names to assist with type inference.
            let from_tables = collect_from_table_names(&create.query);

            let columns = if create.columns.is_empty() {
                // No explicit column list: infer columns from SELECT projection.
                infer_view_columns_from_projection(&create.query, &from_tables, schema)
            } else {
                // Explicit column list: use ViewColumnDef; fall back to positional
                // SELECT projection for types that are not spelled out.
                create
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, col_def)| {
                        let pg_type = if let Some(dt) = &col_def.data_type {
                            data_type_to_pg_type(dt)
                        } else {
                            infer_type_at_projection_pos(i, &create.query, &from_tables, schema)
                                .unwrap_or(PgType::Text)
                        };
                        Column {
                            name: col_def.name.value.clone(),
                            pg_type,
                            is_nullable: true,
                            has_default: false,
                            is_generated: false,
                        }
                    })
                    .collect()
            };

            schema.add_table(Table {
                schema: view_schema,
                name: view_name,
                columns,
                primary_key: None,
                foreign_keys: vec![],
                unique_constraints: vec![],
                is_view: true,
            });
        }
        _ => {
            // DROP, INSERT, etc. are ignored.
        }
    }
}

fn apply_alter_op(table: &mut Table, op: &AlterTableOperation) {
    match op {
        AlterTableOperation::AddColumn { column_def, .. } => {
            table.columns.push(column_from_def(column_def));
        }
        AlterTableOperation::DropColumn { column_names, .. } => {
            for column_name in column_names {
                table.columns.retain(|c| c.name != column_name.value);
            }
        }
        AlterTableOperation::AddConstraint { .. } => {
            tracing::warn!(
                "ALTER TABLE ADD CONSTRAINT is not yet supported in DDL parsing; \
                 constraint will be ignored"
            );
        }
        _ => {}
    }
}

fn column_from_def(col: &ColumnDef) -> Column {
    let is_serial = format!("{}", col.data_type)
        .to_lowercase()
        .contains("serial");
    let is_nullable = !is_serial
        && !col.options.iter().any(|opt| {
            matches!(
                opt,
                ColumnOptionDef { option: ColumnOption::NotNull, .. }
                    | ColumnOptionDef { option: ColumnOption::PrimaryKey(_), .. }
            )
        });
    let has_default = is_serial
        || col.options.iter().any(|opt| {
            matches!(
                opt,
                ColumnOptionDef { option: ColumnOption::Default(_), .. }
            )
        });
    let is_generated = col.options.iter().any(|opt| {
        matches!(
            opt,
            ColumnOptionDef { option: ColumnOption::Generated { .. }, .. }
        )
    });

    let pg_type = data_type_to_pg_type(&col.data_type);

    Column {
        name: col.name.value.clone(),
        pg_type,
        is_nullable,
        has_default,
        is_generated,
    }
}

fn data_type_to_pg_type(dt: &DataType) -> PgType {
    match dt {
        DataType::SmallInt(_) => PgType::SmallInt,
        DataType::Int(_) | DataType::Integer(_) => PgType::Integer,
        DataType::BigInt(_) => PgType::BigInt,
        DataType::Real => PgType::Real,
        DataType::Double(_) | DataType::DoublePrecision => PgType::DoublePrecision,
        DataType::Numeric(_) | DataType::Decimal(_) | DataType::Dec(_) => PgType::Numeric,
        DataType::Boolean => PgType::Boolean,
        DataType::Text => PgType::Text,
        DataType::Varchar(_) | DataType::CharacterVarying(_) => PgType::Varchar,
        DataType::Char(_) | DataType::Character(_) => PgType::Char,
        DataType::Uuid => PgType::Uuid,
        DataType::Date => PgType::Date,
        DataType::Timestamp(_, tz) => {
            if matches!(tz, sqlparser::ast::TimezoneInfo::WithTimeZone) {
                PgType::TimestampTz
            } else {
                PgType::Timestamp
            }
        }
        DataType::Time(_, tz) => {
            if matches!(tz, sqlparser::ast::TimezoneInfo::WithTimeZone) {
                PgType::TimeTz
            } else {
                PgType::Time
            }
        }
        DataType::Interval { .. } => PgType::Interval,
        DataType::JSON => PgType::Json,
        DataType::JSONB => PgType::Jsonb,
        DataType::Bytea => PgType::Bytea,
        DataType::Array(arr_inner) => {
            let inner = match arr_inner {
                sqlparser::ast::ArrayElemTypeDef::AngleBracket(inner_dt)
                | sqlparser::ast::ArrayElemTypeDef::SquareBracket(inner_dt, _)
                | sqlparser::ast::ArrayElemTypeDef::Parenthesis(inner_dt) => {
                    data_type_to_pg_type(inner_dt)
                }
                sqlparser::ast::ArrayElemTypeDef::None => PgType::Text,
            };
            PgType::Array(Box::new(inner))
        }
        _ => {
            // Handle types that sqlparser doesn't have dedicated variants for
            // (e.g. SERIAL, BIGSERIAL) by matching the Display output.
            let name = format!("{dt}");
            PgType::from_sql_name(&name)
        }
    }
}

/// Collect table names referenced in the top-level FROM clause (including
/// joins).
fn collect_from_table_names(query: &sqlparser::ast::Query) -> Vec<String> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return vec![];
    };
    let mut names = Vec::new();
    for twj in &select.from {
        if let TableFactor::Table { name, .. } = &twj.relation {
            names.push(format_object_name(name));
        }
        for join in &twj.joins {
            if let TableFactor::Table { name, .. } = &join.relation {
                names.push(format_object_name(name));
            }
        }
    }
    names
}

/// Infer view columns by scanning the SELECT projection of the defining query.
///
/// Handles explicit column references (`SELECT id, name`), compound identifiers
/// (`SELECT t.id`), aliased expressions (`SELECT id AS user_id`), bare
/// wildcards (`SELECT *`), and qualified wildcards (`SELECT t.*`).
#[expect(
    clippy::too_many_lines,
    reason = "infers columns from all projection expression kinds"
)]
fn infer_view_columns_from_projection(
    query: &sqlparser::ast::Query,
    from_tables: &[String],
    schema: &DatabaseSchema,
) -> Vec<Column> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return vec![];
    };

    let mut columns = Vec::new();

    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(Expr::Identifier(ident)) => {
                let col_name = ident.value.clone();
                let pg_type = find_col_type_in_tables(&col_name, None, from_tables, schema)
                    .unwrap_or(PgType::Text);
                columns.push(Column {
                    name: col_name,
                    pg_type,
                    is_nullable: true,
                    has_default: false,
                    is_generated: false,
                });
            }
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
                if let Some(last) = parts.last() {
                    let col_name = last.value.clone();
                    let qualifier =
                        (parts.len() >= 2).then(|| parts[parts.len() - 2].value.clone());
                    let pg_type = find_col_type_in_tables(
                        &col_name,
                        qualifier.as_deref(),
                        from_tables,
                        schema,
                    )
                    .unwrap_or(PgType::Text);
                    columns.push(Column {
                        name: col_name,
                        pg_type,
                        is_nullable: true,
                        has_default: false,
                        is_generated: false,
                    });
                }
            }
            SelectItem::ExprWithAlias { alias, expr } => {
                let (base_name, qualifier) = match expr {
                    Expr::Identifier(i) => (Some(i.value.clone()), None),
                    Expr::CompoundIdentifier(parts) => {
                        let col = parts.last().map(|i| i.value.clone());
                        let qual = (parts.len() >= 2).then(|| parts[parts.len() - 2].value.clone());
                        (col, qual)
                    }
                    _ => (None, None),
                };
                let pg_type = base_name
                    .as_deref()
                    .and_then(|n| {
                        find_col_type_in_tables(n, qualifier.as_deref(), from_tables, schema)
                    })
                    .unwrap_or(PgType::Text);
                columns.push(Column {
                    name: alias.value.clone(),
                    pg_type,
                    is_nullable: true,
                    has_default: false,
                    is_generated: false,
                });
            }
            SelectItem::Wildcard(_) => {
                // Expand SELECT * to all columns from every FROM-clause table.
                for table_name in from_tables {
                    if let Some(table) = schema.find_table(table_name) {
                        for col in &table.columns {
                            columns.push(Column {
                                name: col.name.clone(),
                                pg_type: col.pg_type.clone(),
                                is_nullable: true,
                                has_default: false,
                                is_generated: false,
                            });
                        }
                    }
                }
            }
            SelectItem::QualifiedWildcard(kind, _) => {
                // Expand table.* to all columns from the specified table.
                let qual: Option<String> = match kind {
                    sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name) => {
                        name.0.last().and_then(|p| match p {
                            sqlparser::ast::ObjectNamePart::Identifier(i) => Some(i.value.clone()),
                            sqlparser::ast::ObjectNamePart::Function(_) => None,
                        })
                    }
                    sqlparser::ast::SelectItemQualifiedWildcardKind::Expr(_) => None,
                };
                if let Some(qual) = qual {
                    for table_name in from_tables {
                        let short = table_name.rsplit('.').next().unwrap_or(table_name.as_str());
                        if (short == qual || table_name.as_str() == qual)
                            && let Some(table) = schema.find_table(table_name)
                        {
                            for col in &table.columns {
                                columns.push(Column {
                                    name: col.name.clone(),
                                    pg_type: col.pg_type.clone(),
                                    is_nullable: true,
                                    has_default: false,
                                    is_generated: false,
                                });
                            }
                        }
                    }
                }
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                let (base_name, qualifier) = match expr {
                    Expr::Identifier(i) => (Some(i.value.clone()), None),
                    Expr::CompoundIdentifier(parts) => {
                        let col = parts.last().map(|i| i.value.clone());
                        let qual = (parts.len() >= 2).then(|| parts[parts.len() - 2].value.clone());
                        (col, qual)
                    }
                    _ => (None, None),
                };
                // Create a column for each alias
                for alias in aliases {
                    let pg_type = base_name
                        .as_deref()
                        .and_then(|n| {
                            find_col_type_in_tables(n, qualifier.as_deref(), from_tables, schema)
                        })
                        .unwrap_or(PgType::Text);
                    columns.push(Column {
                        name: alias.value.clone(),
                        pg_type: pg_type.clone(),
                        is_nullable: true,
                        has_default: false,
                        is_generated: false,
                    });
                }
            }
            SelectItem::UnnamedExpr(_) => {
                // Complex expressions without aliases (functions, arithmetic,
                // etc.) cannot have their column name or type
                // inferred; they are skipped.
            }
        }
    }

    columns
}

/// Infer the type of the view column at the given positional index in the
/// SELECT projection by looking up the underlying column in the FROM tables.
/// Qualifier context (e.g. `t.col`) is preserved to resolve ambiguous columns
/// in joins.
fn infer_type_at_projection_pos(
    pos: usize,
    query: &sqlparser::ast::Query,
    from_tables: &[String],
    schema: &DatabaseSchema,
) -> Option<PgType> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    let item = select.projection.get(pos)?;
    let (col_name, qualifier) = match item {
        SelectItem::UnnamedExpr(Expr::Identifier(ident)) => (Some(ident.value.clone()), None),
        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => {
            let col = parts.last().map(|i| i.value.clone());
            let qual = (parts.len() >= 2).then(|| parts[parts.len() - 2].value.clone());
            (col, qual)
        }
        SelectItem::ExprWithAlias { expr, .. } => match expr {
            Expr::Identifier(i) => (Some(i.value.clone()), None),
            Expr::CompoundIdentifier(parts) => {
                let col = parts.last().map(|i| i.value.clone());
                let qual = (parts.len() >= 2).then(|| parts[parts.len() - 2].value.clone());
                (col, qual)
            }
            _ => (None, None),
        },
        SelectItem::ExprWithAliases { expr, .. } => match expr {
            Expr::Identifier(i) => (Some(i.value.clone()), None),
            Expr::CompoundIdentifier(parts) => {
                let col = parts.last().map(|i| i.value.clone());
                let qual = (parts.len() >= 2).then(|| parts[parts.len() - 2].value.clone());
                (col, qual)
            }
            _ => (None, None),
        },
        _ => (None, None),
    };
    col_name
        .as_deref()
        .and_then(|n| find_col_type_in_tables(n, qualifier.as_deref(), from_tables, schema))
}

/// Search `from_tables` in the schema and return the type of `col_name` if
/// found.
///
/// When `qualifier` is provided the table whose unqualified name matches is
/// tried first, which resolves ambiguous column references in JOINs (e.g.
/// `t.id` where multiple tables share an `id` column).
fn find_col_type_in_tables(
    col_name: &str,
    qualifier: Option<&str>,
    from_tables: &[String],
    schema: &DatabaseSchema,
) -> Option<PgType> {
    if let Some(qual) = qualifier {
        // Prefer the specific table named by the qualifier.
        for table_name in from_tables {
            let short = table_name.rsplit('.').next().unwrap_or(table_name.as_str());
            if (short == qual || table_name.as_str() == qual)
                && let Some(table) = schema.find_table(table_name)
                && let Some(col) = table.find_column(col_name)
            {
                return Some(col.pg_type.clone());
            }
        }
    }
    // Fall back to unqualified search across all FROM tables.
    for table_name in from_tables {
        if let Some(table) = schema.find_table(table_name)
            && let Some(col) = table.find_column(col_name)
        {
            return Some(col.pg_type.clone());
        }
    }
    None
}

fn extract_schema_and_name(name: &ObjectName) -> (String, String) {
    let parts: Vec<String> = name
        .0
        .iter()
        .filter_map(|p| match p {
            sqlparser::ast::ObjectNamePart::Identifier(ident) => Some(ident.value.clone()),
            sqlparser::ast::ObjectNamePart::Function(_) => None,
        })
        .collect();
    match parts.as_slice() {
        [] => ("public".to_string(), String::new()),
        [schema, table] => (schema.clone(), table.clone()),
        [table] => ("public".to_string(), table.clone()),
        other => {
            let len = other.len();
            (other[len - 2].clone(), other[len - 1].clone())
        }
    }
}

fn format_object_name(name: &ObjectName) -> String {
    let (schema, table) = extract_schema_and_name(name);
    format!("{schema}.{table}")
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn parse_simple_create_table() {
        let sql = r"
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                email TEXT,
                created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
            );
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let table = schema.find_table("users").unwrap();

        assert_eq!(table.name, "users");
        assert_eq!(table.schema, "public");
        assert_eq!(table.columns.len(), 4);

        let id_col = table.find_column("id").unwrap();
        assert_eq!(id_col.pg_type, PgType::Integer);
        assert!(!id_col.is_nullable);
        assert!(
            id_col.has_default,
            "SERIAL column should have has_default = true"
        );

        let name_col = table.find_column("name").unwrap();
        assert_eq!(name_col.pg_type, PgType::Varchar);
        assert!(!name_col.is_nullable);

        let email_col = table.find_column("email").unwrap();
        assert!(email_col.is_nullable);

        let created_col = table.find_column("created_at").unwrap();
        assert_eq!(created_col.pg_type, PgType::TimestampTz);
        assert!(created_col.has_default);

        assert!(table.primary_key.is_some());
        assert_eq!(table.primary_key.as_ref().unwrap().columns, vec!["id"]);
    }

    #[test]
    fn parse_foreign_key() {
        let sql = r"
            CREATE TABLE posts (
                id SERIAL PRIMARY KEY,
                user_id INTEGER NOT NULL,
                title TEXT NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id)
            );
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let table = schema.find_table("posts").unwrap();

        assert_eq!(table.foreign_keys.len(), 1);
        let fk = &table.foreign_keys[0];
        assert_eq!(fk.columns, vec!["user_id"]);
        assert_eq!(fk.referenced_table, "users");
        assert_eq!(fk.referenced_columns, vec!["id"]);
    }

    #[test]
    fn parse_multiple_migrations() {
        let m1 = "CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL);".to_string();
        let m2 = "ALTER TABLE users ADD COLUMN email TEXT;".to_string();

        let schema = parse_migrations(&[m1, m2]).unwrap();
        let table = schema.find_table("users").unwrap();
        assert_eq!(table.columns.len(), 3);
        assert!(table.find_column("email").is_some());
    }

    #[test]
    fn parse_alter_table_add_constraint_does_not_panic() {
        let m1 = r"
            CREATE TABLE orders (
                id SERIAL PRIMARY KEY,
                user_id INTEGER NOT NULL
            );
            CREATE TABLE users (
                id SERIAL PRIMARY KEY
            );
        "
        .to_string();
        let m2 =
            "ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users(id);"
                .to_string();

        // Should not panic; the ADD CONSTRAINT is warned but gracefully ignored.
        let schema = parse_migrations(&[m1, m2]).unwrap();
        let table = schema.find_table("orders").unwrap();
        // The FK is not applied via ALTER (not yet supported), so foreign_keys stays
        // empty.
        assert!(table.foreign_keys.is_empty());
    }

    #[test]
    fn parse_create_view() {
        let sql = r"
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                age INTEGER
            );
            CREATE VIEW adult_users AS
                SELECT id, name, age FROM users WHERE age >= 18;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();

        // Base table should still exist and not be a view.
        let table = schema.find_table("users").unwrap();
        assert!(!table.is_view);

        // View should be registered with is_view = true.
        let view = schema.find_table("adult_users").unwrap();
        assert!(view.is_view);
        assert_eq!(view.columns.len(), 3);
        assert!(view.primary_key.is_none());

        // Types should be inferred from the source table.
        let id_col = view.find_column("id").unwrap();
        assert_eq!(id_col.pg_type, PgType::Integer);
        let name_col = view.find_column("name").unwrap();
        assert_eq!(name_col.pg_type, PgType::Varchar);
        let age_col = view.find_column("age").unwrap();
        assert_eq!(age_col.pg_type, PgType::Integer);
    }

    #[test]
    fn parse_create_or_replace_view() {
        let sql = r"
            CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL);
            CREATE OR REPLACE VIEW user_names AS SELECT id, name FROM users;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let view = schema.find_table("user_names").unwrap();
        assert!(view.is_view);
        assert_eq!(view.columns.len(), 2);
    }

    #[test]
    fn parse_create_materialized_view() {
        let sql = r"
            CREATE TABLE orders (
                id SERIAL PRIMARY KEY,
                total NUMERIC NOT NULL
            );
            CREATE MATERIALIZED VIEW order_totals AS SELECT id, total FROM orders;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let view = schema.find_table("order_totals").unwrap();
        assert!(view.is_view);
        assert_eq!(view.columns.len(), 2);
        let total_col = view.find_column("total").unwrap();
        assert_eq!(total_col.pg_type, PgType::Numeric);
    }

    #[test]
    fn parse_view_with_wildcard_select() {
        let sql = r"
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL,
                age INTEGER
            );
            CREATE VIEW all_users AS SELECT * FROM users;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let view = schema.find_table("all_users").unwrap();
        assert!(view.is_view);
        // SELECT * expands to all source table columns.
        assert_eq!(view.columns.len(), 3);
        let id_col = view.find_column("id").unwrap();
        assert_eq!(id_col.pg_type, PgType::Integer);
        let name_col = view.find_column("name").unwrap();
        assert_eq!(name_col.pg_type, PgType::Text);
    }

    #[test]
    fn parse_view_with_qualified_wildcard() {
        let sql = r"
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE VIEW user_view AS SELECT users.* FROM users;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let view = schema.find_table("user_view").unwrap();
        assert!(view.is_view);
        assert_eq!(view.columns.len(), 2);
        let id_col = view.find_column("id").unwrap();
        assert_eq!(id_col.pg_type, PgType::Integer);
    }

    #[test]
    fn parse_view_join_qualifier_disambiguates_columns() {
        // Both tables have an `id` column; qualifier should resolve to the
        // correct type rather than always picking the first table's column.
        let sql = r"
            CREATE TABLE users (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE TABLE posts (
                id SERIAL PRIMARY KEY,
                user_id INTEGER NOT NULL,
                title TEXT NOT NULL
            );
            CREATE VIEW post_details AS
                SELECT posts.id, users.name
                FROM users JOIN posts ON users.id = posts.user_id;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let view = schema.find_table("post_details").unwrap();
        assert!(view.is_view);
        assert_eq!(view.columns.len(), 2);
        let id_col = view.find_column("id").unwrap();
        assert_eq!(id_col.pg_type, PgType::Integer);
        let name_col = view.find_column("name").unwrap();
        assert_eq!(name_col.pg_type, PgType::Text);
    }

    #[test]
    fn parse_view_overwrite_with_or_replace() {
        let sql = r"
            CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL, email TEXT);
            CREATE VIEW user_info AS SELECT id, name FROM users;
            CREATE OR REPLACE VIEW user_info AS SELECT id, name, email FROM users;
        "
        .to_string();

        let schema = parse_migrations(&[sql]).unwrap();
        let view = schema.find_table("user_info").unwrap();
        assert!(view.is_view);
        // After OR REPLACE, the view should have 3 columns.
        assert_eq!(view.columns.len(), 3);
    }
}
