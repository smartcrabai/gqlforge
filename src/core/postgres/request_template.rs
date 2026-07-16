use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use convert_case::{Case, Casing};
use gqlforge_hasher::GqlforgeHasher;

use crate::core::config::PostgresOperation;
use crate::core::has_headers::HasHeaders;
use crate::core::ir::model::{CacheKey, IoId};
use crate::core::mustache::Mustache;
use crate::core::path::{PathString, PathValue, ValueString};

pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(crate) fn quote_qualified_ident(name: &str) -> String {
    name.split('.')
        .map(quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}

/// Template describing how to build a SQL query for a `@postgres` field.
#[derive(Debug, Clone)]
pub struct RequestTemplate {
    pub table: String,
    pub operation: PostgresOperation,
    pub filter: Option<serde_json::Value>,
    pub input: Option<Mustache>,
    pub limit: Option<Mustache>,
    pub offset: Option<Mustache>,
    pub order_by: Option<Mustache>,
    /// Column names (resolved from `DatabaseSchema` at compile time).
    pub columns: Vec<String>,
    /// How the database operation returns its GraphQL value.
    pub result_mode: ResultMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultMode {
    Rows,
    AffectedRows,
}

/// A rendered, ready-to-execute SQL query with parameterised values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedQuery {
    pub sql: String,
    pub params: Vec<Option<String>>,
}

impl Hash for RenderedQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.sql.hash(state);
        self.params.hash(state);
    }
}

impl RequestTemplate {
    /// Render the template against the given context to produce a SQL string
    /// with positional parameters (`$1`, `$2`, ...).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    pub fn render<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        match self.operation {
            PostgresOperation::Select => self.render_select(ctx),
            PostgresOperation::SelectOne => self.render_select_one(ctx),
            PostgresOperation::Insert => self.render_insert(ctx),
            PostgresOperation::Update => self.render_update(ctx),
            PostgresOperation::Delete => self.render_delete(ctx),
            PostgresOperation::Listen => {
                anyhow::bail!(
                    "LISTEN is a subscription-only operation and must not be rendered as SQL"
                );
            }
        }
    }

    fn render_select<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        let cols = self.select_columns();
        let table = quote_qualified_ident(&self.table);
        let mut sql = format!("SELECT {cols} FROM {table}");
        let mut params = Vec::new();

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = self.render_filter(filter, ctx, params.len())?;
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        if let Some(order_by) = &self.order_by {
            let rendered = order_by.render(ctx);
            if !rendered.is_empty() {
                let sanitized = sanitize_order_by(&rendered, &self.columns);
                if !sanitized.is_empty() {
                    let _ = write!(sql, " ORDER BY {sanitized}");
                }
            }
        }

        if let Some(limit) = &self.limit {
            let rendered = limit.render(ctx);
            if !rendered.is_empty() {
                params.push(Some(rendered));
                let _ = write!(sql, " LIMIT ${}", params.len());
            }
        }

        if let Some(offset) = &self.offset {
            let rendered = offset.render(ctx);
            if !rendered.is_empty() {
                params.push(Some(rendered));
                let _ = write!(sql, " OFFSET ${}", params.len());
            }
        }

        Ok(RenderedQuery { sql, params })
    }

    fn render_select_one<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        let cols = self.select_columns();
        let table = quote_qualified_ident(&self.table);
        let mut sql = format!("SELECT {cols} FROM {table}");
        let mut params = Vec::new();

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = self.render_filter(filter, ctx, params.len())?;
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        sql.push_str(" LIMIT 1");
        Ok(RenderedQuery { sql, params })
    }

    fn render_insert<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        let input = self
            .input
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("INSERT requires input"))?;
        let entries = self.resolve_columns(parse_json_object(&render_input(input, ctx)?)?)?;
        if entries.is_empty() {
            anyhow::bail!("INSERT requires at least one field in input");
        }

        let cols: Vec<String> = entries.iter().map(|(k, _)| quote_ident(k)).collect();
        let mut params: Vec<Option<String>> = Vec::new();
        let mut placeholders = Vec::new();

        for (_, v) in &entries {
            params.push(v.clone());
            placeholders.push(format!("${}", params.len()));
        }

        let col_list = cols.join(", ");
        let val_list = placeholders.join(", ");
        let table = quote_qualified_ident(&self.table);
        let mut sql = format!("INSERT INTO {table} ({col_list}) VALUES ({val_list})");
        if self.result_mode == ResultMode::Rows {
            let ret_cols = self.select_columns();
            let _ = write!(sql, " RETURNING {ret_cols}");
        }
        Ok(RenderedQuery { sql, params })
    }

    fn render_update<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        if self.filter.is_none() {
            anyhow::bail!("UPDATE without a filter is not allowed (would affect all rows)");
        }

        let input = self
            .input
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("UPDATE requires input"))?;
        let entries = self.resolve_columns(parse_json_object(&render_input(input, ctx)?)?)?;
        if entries.is_empty() {
            anyhow::bail!("UPDATE requires at least one field in input");
        }

        let mut params: Vec<Option<String>> = Vec::new();
        let mut set_clauses = Vec::new();

        for (k, v) in &entries {
            params.push(v.clone());
            set_clauses.push(format!("{} = ${}", quote_ident(k), params.len()));
        }

        let set_str = set_clauses.join(", ");
        let ret_cols = self.select_columns();
        let table = quote_qualified_ident(&self.table);
        let mut sql = format!("UPDATE {table} SET {set_str}");

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = self.render_filter(filter, ctx, params.len())?;
            if where_clause == "TRUE" {
                anyhow::bail!("UPDATE requires at least one filter condition");
            }
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        let _ = write!(sql, " RETURNING {ret_cols}");
        Ok(RenderedQuery { sql, params })
    }

    fn render_delete<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        if self.filter.is_none() {
            anyhow::bail!("DELETE without a filter is not allowed (would affect all rows)");
        }

        let table = quote_qualified_ident(&self.table);
        let mut sql = format!("DELETE FROM {table}");
        let mut params: Vec<Option<String>> = Vec::new();

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = self.render_filter(filter, ctx, params.len())?;
            if where_clause == "TRUE" {
                anyhow::bail!("DELETE requires at least one filter condition");
            }
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        Ok(RenderedQuery { sql, params })
    }

    /// Parse a JSON filter object into `col = $N` clauses, returning the clause
    /// string and the parameter values.
    fn render_filter<C: PathString + PathValue + HasHeaders>(
        &self,
        filter: &serde_json::Value,
        ctx: &C,
        offset: usize,
    ) -> anyhow::Result<(String, Vec<Option<String>>)> {
        let rendered = render_json_value(filter, ctx)?;
        let entries = self.resolve_columns(parse_json_object(&rendered)?)?;
        let mut params = Vec::new();
        let mut clauses = Vec::new();

        for (k, v) in entries {
            match v {
                Some(v) => {
                    params.push(Some(v));
                    clauses.push(format!("{} = ${}", quote_ident(&k), offset + params.len()));
                }
                None => clauses.push(format!("{} IS NULL", quote_ident(&k))),
            }
        }

        let clause = if clauses.is_empty() {
            "TRUE".to_string()
        } else {
            clauses.join(" AND ")
        };
        Ok((clause, params))
    }

    fn resolve_columns(
        &self,
        entries: Vec<(String, Option<String>)>,
    ) -> anyhow::Result<Vec<(String, Option<String>)>> {
        entries
            .into_iter()
            .map(|(name, value)| Ok((resolve_column_name(&self.columns, &name)?, value)))
            .collect()
    }

    fn select_columns(&self) -> String {
        if self.columns.is_empty() {
            return "*".to_string();
        }

        self.columns
            .iter()
            .flat_map(|column| {
                let quoted = quote_ident(column);
                let graphql_name = column.to_case(Case::Camel);
                if graphql_name == *column
                    || self.columns.iter().any(|other| other == &graphql_name)
                {
                    vec![quoted]
                } else {
                    vec![
                        quoted.clone(),
                        format!("{quoted} AS {}", quote_ident(&graphql_name)),
                    ]
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub(crate) fn cache_key_with_connection<C: PathString + PathValue + HasHeaders>(
        &self,
        ctx: &C,
        connection_id: &str,
    ) -> Option<IoId> {
        let rendered = self.render(ctx).ok()?;
        let mut hasher = GqlforgeHasher::default();
        connection_id.hash(&mut hasher);
        self.result_mode.hash(&mut hasher);
        rendered.hash(&mut hasher);
        Some(IoId::new(hasher.finish()))
    }
}

impl<Ctx: PathString + PathValue + HasHeaders> CacheKey<Ctx> for RequestTemplate {
    fn cache_key(&self, ctx: &Ctx) -> Option<IoId> {
        let rendered = self.render(ctx).ok()?;
        let mut hasher = GqlforgeHasher::default();
        rendered.hash(&mut hasher);
        Some(IoId::new(hasher.finish()))
    }
}

fn resolve_column_name(columns: &[String], name: &str) -> anyhow::Result<String> {
    if columns.is_empty() {
        return Ok(name.to_string());
    }
    if columns.iter().any(|column| column == name) {
        return Ok(name.to_string());
    }

    let mut candidates = columns
        .iter()
        .filter(|column| column.to_case(Case::Camel) == name);
    let Some(column) = candidates.next() else {
        anyhow::bail!("Unknown column in input/filter: {name}");
    };
    if candidates.next().is_some() {
        anyhow::bail!("Ambiguous camelCase column in input/filter: {name}");
    }
    Ok(column.clone())
}

/// Sanitise an ORDER BY clause by validating column names against a whitelist
/// and only allowing `ASC` / `DESC` direction keywords.
fn sanitize_order_by(rendered: &str, columns: &[String]) -> String {
    if columns.is_empty() {
        return String::new();
    }

    rendered
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut tokens = part.split_whitespace();
            let col = resolve_column_name(columns, tokens.next()?).ok()?;
            let dir = tokens.next().map(str::to_uppercase).unwrap_or_default();
            match dir.as_str() {
                "ASC" => Some(format!("{} ASC", quote_ident(&col))),
                "DESC" => Some(format!("{} DESC", quote_ident(&col))),
                "" => Some(quote_ident(&col)),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_input<C: PathString + PathValue>(
    input: &Mustache,
    ctx: &C,
) -> anyhow::Result<serde_json::Value> {
    if let [crate::core::mustache::Segment::Expression(path)] = &input.segments()[..]
        && let Some(value) = ctx.raw_value(path)
    {
        return raw_value_to_json(value);
    }

    let template: serde_json::Value = serde_json::from_str(&input.to_string())
        .map_err(|error| anyhow::anyhow!("Invalid JSON in input/filter: {error}"))?;
    render_json_value(&template, ctx)
}

/// Parse a JSON filter object into SQL parameter values.
fn parse_json_object(value: &serde_json::Value) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object in input/filter, got: {value}"))?;

    Ok(obj
        .iter()
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::Null => None,
                serde_json::Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            };
            (k.clone(), val)
        })
        .collect())
}

fn render_json_value<C: PathString + PathValue>(
    value: &serde_json::Value,
    ctx: &C,
) -> anyhow::Result<serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object
            .iter()
            .map(|(key, value)| Ok((key.clone(), render_json_value(value, ctx)?)))
            .collect(),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| render_json_value(value, ctx))
            .collect::<anyhow::Result<_>>()
            .map(serde_json::Value::Array),
        serde_json::Value::String(template) => {
            let mustache = Mustache::parse(template);
            if let [crate::core::mustache::Segment::Expression(path)] = &mustache.segments()[..]
                && let Some(value) = ctx.raw_value(path)
            {
                return raw_value_to_json(value);
            }
            Ok(serde_json::Value::String(mustache.render(ctx)))
        }
        value => Ok(value.clone()),
    }
}

fn raw_value_to_json(value: ValueString<'_>) -> anyhow::Result<serde_json::Value> {
    match value {
        ValueString::Value(value) => Ok(serde_json::to_value(value.as_ref())?),
        ValueString::String(value) => Ok(serde_json::Value::String(value.into_owned())),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use std::borrow::Cow;

    use http::HeaderMap;

    use super::*;

    struct Ctx {
        value: serde_json::Value,
    }

    impl PathString for Ctx {
        fn path_string<'a, T: AsRef<str>>(&'a self, parts: &'a [T]) -> Option<Cow<'a, str>> {
            self.value.path_string(parts)
        }
    }

    impl PathValue for Ctx {
        fn raw_value<'a, T: AsRef<str>>(
            &'a self,
            parts: &[T],
        ) -> Option<crate::core::path::ValueString<'a>> {
            let value = parts
                .iter()
                .try_fold(&self.value, |value, part| value.get(part.as_ref()))?;
            let value = gqlrs::Value::from_json(value.clone()).ok()?;
            Some(crate::core::path::ValueString::Value(Cow::Owned(value)))
        }
    }

    impl HasHeaders for Ctx {
        fn headers(&self) -> &HeaderMap {
            static EMPTY: std::sync::LazyLock<HeaderMap> = std::sync::LazyLock::new(HeaderMap::new);
            &EMPTY
        }
    }

    #[test]
    fn render_select() {
        let tmpl = RequestTemplate {
            table: "public.users".into(),
            operation: PostgresOperation::Select,
            filter: Some(serde_json::json!({"active": "true"})),
            input: None,
            limit: Some(Mustache::parse("10")),
            offset: Some(Mustache::parse("0")),
            order_by: Some(Mustache::parse("name ASC")),
            columns: vec!["id".into(), "name".into(), "email".into(), "active".into()],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert_eq!(
            rendered.sql,
            r#"SELECT "id", "name", "email", "active" FROM "public"."users" WHERE "active" = $1 ORDER BY "name" ASC LIMIT $2 OFFSET $3"#
        );
        assert_eq!(
            rendered.params,
            vec![
                Some("true".to_string()),
                Some("10".to_string()),
                Some("0".to_string()),
            ]
        );
    }

    #[test]
    fn select_preserves_raw_and_camel_case_column_names() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Select,
            filter: None,
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["metric_value".into()],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert_eq!(
            rendered.sql,
            r#"SELECT "metric_value", "metric_value" AS "metricValue" FROM "metrics""#
        );
    }

    #[test]
    fn render_insert() {
        let tmpl = RequestTemplate {
            table: "public.users".into(),
            operation: PostgresOperation::Insert,
            filter: None,
            input: Some(Mustache::parse(
                r#"{"name": "Alice", "email": "alice@example.com"}"#,
            )),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["id".into(), "name".into(), "email".into()],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert!(rendered.sql.starts_with(r#"INSERT INTO "public"."users""#));
        assert!(rendered.sql.contains("RETURNING"));
        assert_eq!(rendered.params.len(), 2);
    }

    #[test]
    fn render_greptimedb_insert_without_returning() {
        let tmpl = RequestTemplate {
            table: "public.metrics".into(),
            operation: PostgresOperation::Insert,
            filter: None,
            input: Some(Mustache::parse(r#"{"host": "api-1", "value": 1.5}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["host".into(), "value".into()],
            result_mode: ResultMode::AffectedRows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert_eq!(
            rendered.sql,
            r#"INSERT INTO "public"."metrics" ("host", "value") VALUES ($1, $2)"#
        );
        assert_eq!(
            rendered.params,
            vec![Some("api-1".to_string()), Some("1.5".to_string())]
        );
    }

    #[test]
    fn render_delete() {
        let tmpl = RequestTemplate {
            table: "public.users".into(),
            operation: PostgresOperation::Delete,
            filter: Some(serde_json::json!({"id": "42"})),
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec![],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert_eq!(
            rendered.sql,
            r#"DELETE FROM "public"."users" WHERE "id" = $1"#
        );
        assert_eq!(rendered.params, vec![Some("42".to_string())]);
    }

    #[test]
    fn render_insert_maps_camel_case_and_preserves_null() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Insert,
            filter: None,
            input: Some(Mustache::parse(r#"{"metricValue": 1.5, "note": null}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["metric_value".into(), "note".into()],
            result_mode: ResultMode::AffectedRows,
        };

        let rendered = tmpl
            .render(&Ctx { value: serde_json::Value::Null })
            .unwrap();

        assert_eq!(
            rendered.sql,
            r#"INSERT INTO "metrics" ("metric_value", "note") VALUES ($1, $2)"#
        );
        assert_eq!(rendered.params, vec![Some("1.5".to_string()), None]);
    }

    #[test]
    fn render_filter_maps_camel_case_null_to_is_null() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Select,
            filter: Some(serde_json::json!({"metricValue": null})),
            input: None,
            limit: None,
            offset: None,
            order_by: Some(Mustache::parse("metricValue DESC")),
            columns: vec!["metric_value".into()],
            result_mode: ResultMode::Rows,
        };

        let rendered = tmpl
            .render(&Ctx { value: serde_json::Value::Null })
            .unwrap();

        assert_eq!(
            rendered.sql,
            r#"SELECT "metric_value", "metric_value" AS "metricValue" FROM "metrics" WHERE "metric_value" IS NULL ORDER BY "metric_value" DESC"#
        );
        assert!(rendered.params.is_empty());
    }

    #[test]
    fn render_delete_rejects_empty_filter() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Delete,
            filter: Some(serde_json::json!({})),
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["host".into()],
            result_mode: ResultMode::AffectedRows,
        };

        let error = tmpl
            .render(&Ctx { value: serde_json::Value::Null })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("DELETE requires at least one filter condition")
        );
    }

    #[test]
    fn insert_unknown_column_rejected() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Insert,
            filter: None,
            input: Some(Mustache::parse(r#"{"name": "Alice", "bogus": "bad"}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["id".into(), "name".into(), "email".into()],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unknown column in input/filter: bogus"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn update_unknown_column_rejected() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Update,
            filter: Some(serde_json::json!({"id": "1"})),
            input: Some(Mustache::parse(r#"{"bogus": "bad"}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["id".into(), "name".into(), "email".into()],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unknown column in input/filter: bogus"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn update_without_filter_rejected() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Update,
            filter: None,
            input: Some(Mustache::parse(r#"{"name": "Alice"}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec![],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string().contains("UPDATE without a filter"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn delete_without_filter_rejected() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Delete,
            filter: None,
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec![],
            result_mode: ResultMode::Rows,
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string().contains("DELETE without a filter"),
            "unexpected error: {err}"
        );
    }
    #[test]
    fn dynamic_filter_preserves_quoted_strings_and_nulls() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Select,
            filter: Some(serde_json::json!({
                "host": "{{.args.host}}",
                "note": "{{.args.note}}"
            })),
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["host".into(), "note".into()],
            result_mode: ResultMode::Rows,
        };

        let rendered = tmpl
            .render(&Ctx {
                value: serde_json::json!({"args": {"host": "api\"1", "note": null}}),
            })
            .unwrap();

        assert_eq!(
            rendered.sql,
            r#"SELECT "host", "note" FROM "metrics" WHERE "host" = $1 AND "note" IS NULL"#
        );
        assert_eq!(rendered.params, vec![Some("api\"1".to_string())]);
    }

    #[test]
    fn dynamic_input_preserves_quoted_strings_and_nulls() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Insert,
            filter: None,
            input: Some(Mustache::parse("{{.args.input}}")),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["host".into(), "note".into()],
            result_mode: ResultMode::AffectedRows,
        };

        let rendered = tmpl
            .render(&Ctx {
                value: serde_json::json!({"args": {"input": {"host": "api\"1", "note": null}}}),
            })
            .unwrap();

        assert_eq!(
            rendered.sql,
            r#"INSERT INTO "metrics" ("host", "note") VALUES ($1, $2)"#
        );
        assert_eq!(rendered.params, vec![Some("api\"1".to_string()), None]);
    }

    #[test]
    fn update_rejects_empty_filter() {
        let tmpl = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Update,
            filter: Some(serde_json::json!({})),
            input: Some(Mustache::parse(r#"{"host": "api-1"}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["host".into()],
            result_mode: ResultMode::Rows,
        };

        let error = tmpl
            .render(&Ctx { value: serde_json::Value::Null })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("UPDATE requires at least one filter condition")
        );
    }

    #[test]
    fn cache_key_includes_connection_and_result_mode() {
        let ctx = Ctx { value: serde_json::Value::Null };
        let rows = RequestTemplate {
            table: "metrics".into(),
            operation: PostgresOperation::Delete,
            filter: Some(serde_json::json!({"host": "api-1"})),
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["host".into()],
            result_mode: ResultMode::Rows,
        };
        let affected_rows =
            RequestTemplate { result_mode: ResultMode::AffectedRows, ..rows.clone() };

        assert_ne!(
            rows.cache_key_with_connection(&ctx, "postgres"),
            rows.cache_key_with_connection(&ctx, "greptimedb")
        );
        assert_ne!(
            rows.cache_key_with_connection(&ctx, "postgres"),
            affected_rows.cache_key_with_connection(&ctx, "postgres")
        );
    }
}
