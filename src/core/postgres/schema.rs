use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::core::merge_right::MergeRight;

/// `PostgreSQL` type -> GraphQL scalar mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PgType {
    SmallInt,
    Integer,
    BigInt,
    Real,
    DoublePrecision,
    Numeric,
    Boolean,
    Text,
    Varchar,
    Char,
    Uuid,
    Date,
    Timestamp,
    TimestampTz,
    Time,
    TimeTz,
    Interval,
    Json,
    Jsonb,
    Bytea,
    Inet,
    Cidr,
    MacAddr,
    /// Array of another type.
    Array(Box<PgType>),
    // Range types (built-in)
    Int4Range,
    Int8Range,
    NumRange,
    TsRange,
    TstzRange,
    DateRange,
    /// Fallback for unrecognised types.
    Other(String),
}

impl PgType {
    /// Map a `PostgreSQL` type name (as returned by `information_schema` or
    /// `sqlparser`) to a `PgType`.
    #[must_use]
    pub fn from_sql_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "smallint" | "int2" | "smallserial" | "serial2" => PgType::SmallInt,
            "integer" | "int" | "int4" | "serial" | "serial4" => PgType::Integer,
            "bigint" | "int8" | "bigserial" | "serial8" => PgType::BigInt,
            "real" | "float4" => PgType::Real,
            "double precision" | "float8" => PgType::DoublePrecision,
            "numeric" | "decimal" => PgType::Numeric,
            "boolean" | "bool" => PgType::Boolean,
            "text" => PgType::Text,
            "character varying" | "varchar" => PgType::Varchar,
            "character" | "char" => PgType::Char,
            "uuid" => PgType::Uuid,
            "date" => PgType::Date,
            "timestamp" | "timestamp without time zone" => PgType::Timestamp,
            "timestamp with time zone" | "timestamptz" => PgType::TimestampTz,
            "time" | "time without time zone" => PgType::Time,
            "time with time zone" | "timetz" => PgType::TimeTz,
            "interval" => PgType::Interval,
            "json" => PgType::Json,
            "jsonb" => PgType::Jsonb,
            "bytea" => PgType::Bytea,
            "inet" => PgType::Inet,
            "cidr" => PgType::Cidr,
            "macaddr" | "macaddr8" => PgType::MacAddr,
            "int4range" => PgType::Int4Range,
            "int8range" => PgType::Int8Range,
            "numrange" => PgType::NumRange,
            "tsrange" => PgType::TsRange,
            "tstzrange" => PgType::TstzRange,
            "daterange" => PgType::DateRange,
            other => {
                if let Some(inner) = other.strip_suffix("[]") {
                    PgType::Array(Box::new(PgType::from_sql_name(inner)))
                } else {
                    PgType::Other(other.to_string())
                }
            }
        }
    }

    /// The corresponding GraphQL scalar name. `Numeric` uses `String` to
    /// preserve its exact decoded precision.
    #[must_use]
    pub fn graphql_scalar(&self) -> &str {
        match self {
            PgType::SmallInt | PgType::Integer => "Int",
            PgType::Real | PgType::DoublePrecision => "Float",
            PgType::Boolean => "Boolean",
            PgType::Uuid => "ID",
            PgType::Json | PgType::Jsonb | PgType::Array(_) => "JSON",
            PgType::Date | PgType::Timestamp | PgType::TimestampTz => "DateTime",
            _ => "String",
        }
    }
}

impl fmt::Display for PgType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgType::Array(inner) => write!(f, "{inner}[]"),
            PgType::Other(name) => write!(f, "{name}"),
            _ => write!(f, "{self:?}"),
        }
    }
}

/// A single column in a table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub pg_type: PgType,
    pub is_nullable: bool,
    pub has_default: bool,
    pub is_generated: bool,
}

/// Primary key definition (supports composite keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryKey {
    pub columns: Vec<String>,
}

/// Foreign key referencing another table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

/// Unique constraint on one or more columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniqueConstraint {
    pub columns: Vec<String>,
}

/// Full metadata for a single table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
    pub primary_key: Option<PrimaryKey>,
    pub foreign_keys: Vec<ForeignKey>,
    pub unique_constraints: Vec<UniqueConstraint>,
    /// True if this relation is a VIEW; false for a base table.
    /// Note: views are not necessarily read-only - simple views may be
    /// directly updatable, and any view can be made writable via INSTEAD OF
    /// triggers or rules. This flag indicates the relation type only, not
    /// guaranteed write-safety.
    #[serde(default, skip_serializing_if = "crate::core::is_default")]
    pub is_view: bool,
}

impl Table {
    #[must_use]
    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }

    #[must_use]
    pub fn find_column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// The complete database schema.
/// Key is `"schema.table_name"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseSchema {
    pub tables: BTreeMap<String, Table>,
}

impl DatabaseSchema {
    #[must_use]
    pub fn new() -> Self {
        Self { tables: BTreeMap::new() }
    }

    pub fn add_table(&mut self, table: Table) {
        let key = table.qualified_name();
        self.tables.insert(key, table);
    }

    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.tables.extend(other.tables);
        self
    }

    /// Look up a table by a qualified name, `public` fallback, or an
    /// unambiguous table name in another schema.
    #[must_use]
    pub fn find_table(&self, name: &str) -> Option<&Table> {
        self.tables.get(name).or_else(|| {
            if name.contains('.') {
                return None;
            }

            self.tables.get(&format!("public.{name}")).or_else(|| {
                let mut matches = self.tables.values().filter(|table| table.name == name);
                let table = matches.next()?;
                matches.next().is_none().then_some(table)
            })
        })
    }
}

impl MergeRight for DatabaseSchema {
    fn merge_right(mut self, other: Self) -> Self {
        self.tables.extend(other.tables);
        self
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    #[test]
    fn pg_type_from_sql_name_range_types() {
        assert_eq!(PgType::from_sql_name("int4range"), PgType::Int4Range);
        assert_eq!(PgType::from_sql_name("int8range"), PgType::Int8Range);
        assert_eq!(PgType::from_sql_name("numrange"), PgType::NumRange);
        assert_eq!(PgType::from_sql_name("tsrange"), PgType::TsRange);
        assert_eq!(PgType::from_sql_name("tstzrange"), PgType::TstzRange);
        assert_eq!(PgType::from_sql_name("daterange"), PgType::DateRange);
    }

    #[test]
    fn pg_type_from_sql_name_range_types_case_insensitive() {
        assert_eq!(PgType::from_sql_name("INT4RANGE"), PgType::Int4Range);
        assert_eq!(PgType::from_sql_name("NUMRANGE"), PgType::NumRange);
        assert_eq!(PgType::from_sql_name("TSTZRANGE"), PgType::TstzRange);
    }

    #[test]
    fn graphql_scalar_range_types_return_string() {
        assert_eq!(PgType::Int4Range.graphql_scalar(), "String");
        assert_eq!(PgType::Int8Range.graphql_scalar(), "String");
        assert_eq!(PgType::NumRange.graphql_scalar(), "String");
        assert_eq!(PgType::TsRange.graphql_scalar(), "String");
        assert_eq!(PgType::TstzRange.graphql_scalar(), "String");
        assert_eq!(PgType::DateRange.graphql_scalar(), "String");
    }

    #[test]
    fn pg_type_display_range_types() {
        assert_eq!(PgType::Int4Range.to_string(), "Int4Range");
        assert_eq!(PgType::Int8Range.to_string(), "Int8Range");
        assert_eq!(PgType::NumRange.to_string(), "NumRange");
        assert_eq!(PgType::TsRange.to_string(), "TsRange");
        assert_eq!(PgType::TstzRange.to_string(), "TstzRange");
        assert_eq!(PgType::DateRange.to_string(), "DateRange");
    }

    #[test]
    fn pg_type_from_sql_name() {
        assert_eq!(PgType::from_sql_name("integer"), PgType::Integer);
        assert_eq!(PgType::from_sql_name("INT"), PgType::Integer);
        assert_eq!(PgType::from_sql_name("varchar"), PgType::Varchar);
        assert_eq!(PgType::from_sql_name("boolean"), PgType::Boolean);
        assert_eq!(PgType::from_sql_name("timestamptz"), PgType::TimestampTz);
        assert_eq!(
            PgType::from_sql_name("integer[]"),
            PgType::Array(Box::new(PgType::Integer))
        );
        assert_eq!(
            PgType::from_sql_name("custom_type"),
            PgType::Other("custom_type".into())
        );
    }

    #[test]
    fn graphql_scalar_mapping() {
        assert_eq!(PgType::Integer.graphql_scalar(), "Int");
        assert_eq!(PgType::DoublePrecision.graphql_scalar(), "Float");
        assert_eq!(PgType::Boolean.graphql_scalar(), "Boolean");
        assert_eq!(PgType::Text.graphql_scalar(), "String");
        assert_eq!(PgType::Uuid.graphql_scalar(), "ID");
        assert_eq!(PgType::Jsonb.graphql_scalar(), "JSON");
        assert_eq!(PgType::Numeric.graphql_scalar(), "String");
    }

    #[test]
    fn database_schema_find_table() {
        let mut schema = DatabaseSchema::new();
        schema.add_table(Table {
            schema: "public".into(),
            name: "users".into(),
            columns: vec![],
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: false,
        });

        assert!(schema.find_table("public.users").is_some());
        assert!(schema.find_table("users").is_some());
        assert!(schema.find_table("nonexistent").is_none());
    }

    #[test]
    fn database_schema_finds_unambiguous_non_public_table() {
        let mut schema = DatabaseSchema::new();
        schema.add_table(Table {
            schema: "metrics".into(),
            name: "events".into(),
            columns: vec![],
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: false,
        });

        assert!(schema.find_table("events").is_some());

        schema.add_table(Table {
            schema: "archive".into(),
            name: "events".into(),
            columns: vec![],
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: false,
        });
        assert!(schema.find_table("events").is_none());
    }

    #[test]
    fn database_schema_view_helpers() {
        let mut schema = DatabaseSchema::new();
        schema.add_table(Table {
            schema: "public".into(),
            name: "user_summary".into(),
            columns: vec![],
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: true,
        });

        let view = schema.find_table("user_summary").unwrap();
        assert!(view.is_view);

        schema.add_table(Table {
            schema: "public".into(),
            name: "users".into(),
            columns: vec![],
            primary_key: None,
            foreign_keys: vec![],
            unique_constraints: vec![],
            is_view: false,
        });

        let table = schema.find_table("users").unwrap();
        assert!(!table.is_view);
    }
}
