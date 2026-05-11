use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use gqlforge_hasher::GqlforgeHasher;

use crate::core::config::PostgresOperation;
use crate::core::has_headers::HasHeaders;
use crate::core::ir::model::{CacheKey, IoId};
use crate::core::mustache::Mustache;
use crate::core::path::PathString;

pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Template describing how to build a SQL query for a `@postgres` field.
#[derive(Debug, Clone)]
pub struct RequestTemplate {
    pub table: String,
    pub operation: PostgresOperation,
    pub filter: Option<Mustache>,
    pub input: Option<Mustache>,
    pub limit: Option<Mustache>,
    pub offset: Option<Mustache>,
    pub order_by: Option<Mustache>,
    /// Column names (resolved from `DatabaseSchema` at compile time).
    pub columns: Vec<String>,
}

/// A rendered, ready-to-execute SQL query with parameterised values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedQuery {
    pub sql: String,
    pub params: Vec<String>,
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
    pub fn render<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedQuery> {
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

    fn render_select<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedQuery> {
        let cols = self.select_columns();
        let table = quote_ident(&self.table);
        let mut sql = format!("SELECT {cols} FROM {table}");
        let mut params = Vec::new();

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = Self::render_filter(filter, ctx, params.len())?;
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
                params.push(rendered);
                let _ = write!(sql, " LIMIT ${}", params.len());
            }
        }

        if let Some(offset) = &self.offset {
            let rendered = offset.render(ctx);
            if !rendered.is_empty() {
                params.push(rendered);
                let _ = write!(sql, " OFFSET ${}", params.len());
            }
        }

        Ok(RenderedQuery { sql, params })
    }

    fn render_select_one<C: PathString + HasHeaders>(
        &self,
        ctx: &C,
    ) -> anyhow::Result<RenderedQuery> {
        let cols = self.select_columns();
        let table = quote_ident(&self.table);
        let mut sql = format!("SELECT {cols} FROM {table}");
        let mut params = Vec::new();

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = Self::render_filter(filter, ctx, params.len())?;
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        sql.push_str(" LIMIT 1");
        Ok(RenderedQuery { sql, params })
    }

    fn render_insert<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedQuery> {
        let input_json = self
            .input
            .as_ref()
            .map(|m| m.render(ctx))
            .unwrap_or_default();

        let entries = parse_json_object(&input_json)?;
        if entries.is_empty() {
            anyhow::bail!("INSERT requires at least one field in input");
        }

        if !self.columns.is_empty() {
            let unknown: Vec<&str> = entries
                .iter()
                .filter(|(k, _)| !self.columns.iter().any(|c| c == k))
                .map(|(k, _)| k.as_str())
                .collect();
            if !unknown.is_empty() {
                anyhow::bail!("Unknown column(s) in INSERT input: {}", unknown.join(", "));
            }
        }

        let cols: Vec<String> = entries.iter().map(|(k, _)| quote_ident(k)).collect();
        let mut params: Vec<String> = Vec::new();
        let mut placeholders = Vec::new();

        for (_, v) in &entries {
            params.push(v.clone());
            placeholders.push(format!("${}", params.len()));
        }

        let col_list = cols.join(", ");
        let val_list = placeholders.join(", ");
        let ret_cols = self.select_columns();
        let table = quote_ident(&self.table);

        let sql =
            format!("INSERT INTO {table} ({col_list}) VALUES ({val_list}) RETURNING {ret_cols}");
        Ok(RenderedQuery { sql, params })
    }

    fn render_update<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedQuery> {
        if self.filter.is_none() {
            anyhow::bail!("UPDATE without a filter is not allowed (would affect all rows)");
        }

        let input_json = self
            .input
            .as_ref()
            .map(|m| m.render(ctx))
            .unwrap_or_default();

        let entries = parse_json_object(&input_json)?;
        if entries.is_empty() {
            anyhow::bail!("UPDATE requires at least one field in input");
        }

        if !self.columns.is_empty() {
            let unknown: Vec<&str> = entries
                .iter()
                .filter(|(k, _)| !self.columns.iter().any(|c| c == k))
                .map(|(k, _)| k.as_str())
                .collect();
            if !unknown.is_empty() {
                anyhow::bail!("Unknown column(s) in UPDATE input: {}", unknown.join(", "));
            }
        }

        let mut params: Vec<String> = Vec::new();
        let mut set_clauses = Vec::new();

        for (k, v) in &entries {
            params.push(v.clone());
            set_clauses.push(format!("{} = ${}", quote_ident(k), params.len()));
        }

        let set_str = set_clauses.join(", ");
        let ret_cols = self.select_columns();
        let table = quote_ident(&self.table);
        let mut sql = format!("UPDATE {table} SET {set_str}");

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = Self::render_filter(filter, ctx, params.len())?;
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        let _ = write!(sql, " RETURNING {ret_cols}");
        Ok(RenderedQuery { sql, params })
    }

    fn render_delete<C: PathString + HasHeaders>(&self, ctx: &C) -> anyhow::Result<RenderedQuery> {
        if self.filter.is_none() {
            anyhow::bail!("DELETE without a filter is not allowed (would affect all rows)");
        }

        let table = quote_ident(&self.table);
        let mut sql = format!("DELETE FROM {table}");
        let mut params = Vec::new();

        if let Some(filter) = &self.filter {
            let (where_clause, where_params) = Self::render_filter(filter, ctx, params.len())?;
            let _ = write!(sql, " WHERE {where_clause}");
            params.extend(where_params);
        }

        Ok(RenderedQuery { sql, params })
    }

    /// Parse a JSON filter object into `col = $N` clauses, returning the clause
    /// string and the parameter values.
    fn render_filter<C: PathString + HasHeaders>(
        filter: &Mustache,
        ctx: &C,
        offset: usize,
    ) -> anyhow::Result<(String, Vec<String>)> {
        let rendered = filter.render(ctx);
        let entries = parse_json_object(&rendered)?;
        let mut params = Vec::new();
        let mut clauses = Vec::new();

        for (k, v) in entries {
            params.push(v);
            clauses.push(format!("{} = ${}", quote_ident(&k), offset + params.len()));
        }

        let clause = if clauses.is_empty() {
            "TRUE".to_string()
        } else {
            clauses.join(" AND ")
        };
        Ok((clause, params))
    }

    fn select_columns(&self) -> String {
        if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

impl<Ctx: PathString + HasHeaders> CacheKey<Ctx> for RequestTemplate {
    fn cache_key(&self, ctx: &Ctx) -> Option<IoId> {
        let rendered = self.render(ctx).ok()?;
        let mut hasher = GqlforgeHasher::default();
        rendered.hash(&mut hasher);
        Some(IoId::new(hasher.finish()))
    }
}

/// Sanitise an ORDER BY clause by validating column names against a whitelist
/// and only allowing `ASC` / `DESC` direction keywords.
fn sanitize_order_by(rendered: &str, columns: &[String]) -> String {
    rendered
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut tokens = part.split_whitespace();
            let col = tokens.next()?;
            if !columns.iter().any(|c| c == col) {
                return None;
            }
            let dir = tokens.next().map(str::to_uppercase).unwrap_or_default();
            match dir.as_str() {
                "ASC" => Some(format!("{} ASC", quote_ident(col))),
                "DESC" => Some(format!("{} DESC", quote_ident(col))),
                "" => Some(quote_ident(col)),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse a simple JSON object `{"k":"v", ...}` into key-value pairs.
/// Values are stringified (quotes stripped for simple strings).
fn parse_json_object(json_str: &str) -> anyhow::Result<Vec<(String, String)>> {
    let value: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON in input/filter: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected JSON object in input/filter, got: {value}"))?;

    Ok(obj
        .iter()
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), val)
        })
        .collect())
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

    impl HasHeaders for Ctx {
        fn headers(&self) -> &HeaderMap {
            static EMPTY: std::sync::LazyLock<HeaderMap> = std::sync::LazyLock::new(HeaderMap::new);
            &EMPTY
        }
    }

    #[test]
    fn render_select() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Select,
            filter: Some(Mustache::parse(r#"{"active": "true"}"#)),
            input: None,
            limit: Some(Mustache::parse("10")),
            offset: Some(Mustache::parse("0")),
            order_by: Some(Mustache::parse("name ASC")),
            columns: vec!["id".into(), "name".into(), "email".into()],
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert_eq!(
            rendered.sql,
            r#"SELECT "id", "name", "email" FROM "users" WHERE "active" = $1 ORDER BY "name" ASC LIMIT $2 OFFSET $3"#
        );
        assert_eq!(rendered.params, vec!["true", "10", "0"]);
    }

    #[test]
    fn render_insert() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Insert,
            filter: None,
            input: Some(Mustache::parse(
                r#"{"name": "Alice", "email": "alice@example.com"}"#,
            )),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["id".into(), "name".into(), "email".into()],
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert!(rendered.sql.starts_with(r#"INSERT INTO "users""#));
        assert!(rendered.sql.contains("RETURNING"));
        assert_eq!(rendered.params.len(), 2);
    }

    #[test]
    fn render_delete() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Delete,
            filter: Some(Mustache::parse(r#"{"id": "42"}"#)),
            input: None,
            limit: None,
            offset: None,
            order_by: None,
            columns: vec![],
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let rendered = tmpl.render(&ctx).unwrap();

        assert_eq!(rendered.sql, r#"DELETE FROM "users" WHERE "id" = $1"#);
        assert_eq!(rendered.params, vec!["42"]);
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
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unknown column(s) in INSERT input"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn update_unknown_column_rejected() {
        let tmpl = RequestTemplate {
            table: "users".into(),
            operation: PostgresOperation::Update,
            filter: Some(Mustache::parse(r#"{"id": "1"}"#)),
            input: Some(Mustache::parse(r#"{"bogus": "bad"}"#)),
            limit: None,
            offset: None,
            order_by: None,
            columns: vec!["id".into(), "name".into(), "email".into()],
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unknown column(s) in UPDATE input"),
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
        };

        let ctx = Ctx { value: serde_json::Value::Null };
        let err = tmpl.render(&ctx).unwrap_err();
        assert!(
            err.to_string().contains("DELETE without a filter"),
            "unexpected error: {err}"
        );
    }
}
