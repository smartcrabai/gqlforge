use std::collections::BTreeSet;
use std::sync::Arc;

use async_graphql::ValidationMode;
use async_graphql::dynamic::{Schema, SchemaBuilder};
use async_graphql::extensions::ApolloTracing;
use derive_setters::Setters;

use super::directive::Directive;
use super::telemetry::Telemetry;
use super::{GlobalTimeout, Index};
use crate::core::blueprint::{Server, Upstream};
use crate::core::ir::model::IR;
use crate::core::schema_extension::SchemaExtension;
use crate::core::{Type, scalar};

/// Blueprint is an intermediary representation that allows us to generate
/// graphQL APIs. It can only be generated from a valid Config.
/// It allows us to choose a different GraphQL Backend, without re-writing all
/// orchestration logic. It's not optimized for REST APIs (yet).
#[derive(Clone, Debug, Default, Setters)]
pub struct Blueprint {
    pub definitions: Vec<Definition>,
    pub schema: SchemaDefinition,
    pub server: Server,
    pub upstream: Upstream,
    pub telemetry: Telemetry,
    /// `PostgreSQL` connection definitions: `(connection_id, connection_spec)`.
    #[setters(skip)]
    pub postgres_connections: Vec<(String, PostgresConnectionSpec)>,
}

/// Describes how to connect to a PostgreSQL-compatible database.
#[derive(Clone, Debug)]
pub enum PostgresConnectionSpec {
    /// Standard `PostgreSQL` connection URL (e.g.
    /// `postgresql://user:pass@host/db`).
    Url(String),
    /// Aurora DSQL connection using AWS IAM authentication.
    AuroraDsql {
        /// Cluster endpoint hostname (no scheme).
        endpoint: String,
        /// AWS region (e.g. `us-east-1`).
        region: String,
        /// Use admin token if true; regular user token otherwise.
        admin: bool,
    },
}

#[derive(Clone, Debug)]
pub enum Definition {
    Interface(InterfaceTypeDefinition),
    Object(ObjectTypeDefinition),
    InputObject(InputObjectTypeDefinition),
    Scalar(ScalarTypeDefinition),
    Enum(EnumTypeDefinition),
    Union(UnionTypeDefinition),
}
impl Definition {
    /// gets the name of the definition
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Definition::Interface(def) => &def.name,
            Definition::Object(def) => &def.name,
            Definition::InputObject(def) => &def.name,
            Definition::Scalar(def) => &def.name,
            Definition::Enum(def) => &def.name,
            Definition::Union(def) => &def.name,
        }
    }

    /// gets directives associated with definition
    #[must_use]
    pub fn directives(&self) -> &[Directive] {
        match self {
            Definition::Interface(def) => &def.directives,
            Definition::Object(def) => &def.directives,
            Definition::InputObject(def) => &def.directives,
            Definition::Scalar(def) => &def.directives,
            Definition::Enum(def) => &def.directives,
            Definition::Union(def) => &def.directives,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InterfaceTypeDefinition {
    pub name: String,
    pub fields: Vec<FieldDefinition>,
    pub description: Option<String>,
    pub implements: BTreeSet<String>,
    pub directives: Vec<Directive>,
}

#[derive(Clone, Debug)]
pub struct ObjectTypeDefinition {
    pub name: String,
    pub fields: Vec<FieldDefinition>,
    pub description: Option<String>,
    pub implements: BTreeSet<String>,
    pub directives: Vec<Directive>,
}

#[derive(Clone, Debug)]
pub struct InputObjectTypeDefinition {
    pub name: String,
    pub fields: Vec<InputFieldDefinition>,
    pub description: Option<String>,
    pub directives: Vec<Directive>,
}

#[derive(Clone, Debug)]
pub struct EnumTypeDefinition {
    pub name: String,
    pub directives: Vec<Directive>,
    pub description: Option<String>,
    pub enum_values: Vec<EnumValueDefinition>,
}

#[derive(Clone, Debug)]
pub struct EnumValueDefinition {
    pub description: Option<String>,
    pub name: String,
    pub directives: Vec<Directive>,
    pub alias: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SchemaDefinition {
    pub query: String,
    pub mutation: Option<String>,
    pub subscription: Option<String>,
    pub directives: Vec<Directive>,
}

#[derive(Clone, Debug)]
pub struct InputFieldDefinition {
    pub name: String,
    pub of_type: Type,
    pub default_value: Option<serde_json::Value>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Setters, Default)]
pub struct FieldDefinition {
    pub name: String,
    pub args: Vec<InputFieldDefinition>,
    pub of_type: Type,
    pub resolver: Option<IR>,
    pub directives: Vec<Directive>,
    pub description: Option<String>,
    pub default_value: Option<serde_json::Value>,
}

impl FieldDefinition {
    ///
    /// Transforms the current expression if it exists on the provided field.
    pub fn map_expr<F: FnOnce(IR) -> IR>(&mut self, wrapper: F) {
        if let Some(resolver) = self.resolver.take() {
            self.resolver = Some(wrapper(resolver));
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScalarTypeDefinition {
    pub name: String,
    pub directives: Vec<Directive>,
    pub description: Option<String>,
    pub scalar: scalar::Scalar,
}

#[derive(Clone, Debug)]
pub struct UnionTypeDefinition {
    pub name: String,
    pub directives: Vec<Directive>,
    pub description: Option<String>,
    pub types: BTreeSet<String>,
}

///
/// Controls the kind of blueprint that is generated.
#[derive(Clone, Default, Setters)]
pub struct SchemaModifiers {
    /// If true, the generated schema will not have any resolvers.
    pub no_resolver: bool,
    /// List of extensions to add to the schema.
    pub extensions: Arc<Vec<SchemaExtension>>,
}

impl SchemaModifiers {
    #[must_use]
    pub fn with_no_resolver(mut self) -> Self {
        self.no_resolver = true;
        self
    }
}

impl Blueprint {
    pub fn query(&self) -> String {
        self.schema.query.clone()
    }

    pub fn mutation(&self) -> Option<String> {
        self.schema.mutation.clone()
    }

    pub fn subscription(&self) -> Option<String> {
        self.schema.subscription.clone()
    }

    fn drop_resolvers(mut self) -> Self {
        for def in &mut self.definitions {
            if let Definition::Object(def) = def {
                for field in &mut def.fields {
                    field.resolver = None;
                }
            }
        }

        self
    }

    ///
    /// This function is used to generate a schema from a blueprint.
    pub fn to_schema(&self) -> Schema {
        self.to_schema_with(&SchemaModifiers::default())
    }

    ///
    /// This function is used to generate a schema from a blueprint.
    /// The generated schema can be modified using the `SchemaModifiers`.
    ///
    /// # Panics
    ///
    /// Panics if an internal assertion fails.
    pub fn to_schema_with(&self, schema_modifiers: &SchemaModifiers) -> Schema {
        let blueprint = if schema_modifiers.no_resolver {
            self.clone().drop_resolvers()
        } else {
            self.clone()
        };

        let server = &blueprint.server;
        let mut schema = SchemaBuilder::from(&blueprint);

        if server.enable_apollo_tracing {
            schema = schema.extension(ApolloTracing);
        }

        if server.global_response_timeout > 0 {
            schema = schema
                .data(async_graphql::Value::from(server.global_response_timeout))
                .extension(GlobalTimeout);
        }

        if server.get_enable_query_validation() || schema_modifiers.no_resolver {
            schema = schema.validation_mode(ValidationMode::Strict);
        } else {
            schema = schema.validation_mode(ValidationMode::Fast);
        }

        if !server.get_enable_introspection() || schema_modifiers.no_resolver {
            schema = schema.disable_introspection();
        }

        if server.limit_complexity > 0 {
            schema = schema.limit_complexity(server.limit_complexity);
        }
        if server.limit_depth > 0 {
            schema = schema.limit_depth(server.limit_depth);
        }
        if server.limit_directives > 0 {
            schema = schema.limit_directives(server.limit_directives);
        }

        for extension in schema_modifiers.extensions.iter().cloned() {
            schema = schema.extension(extension);
        }

        // We should safely assume the blueprint is correct and,
        // generation of schema cannot fail.
        #[expect(
            clippy::unwrap_used,
            reason = "schema.finish() cannot fail for a valid blueprint"
        )]
        schema.finish().unwrap()
    }

    pub fn index(&self) -> Index {
        Index::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_connection_spec_aurora_dsql_stores_fields() {
        let endpoint = "cluster123.dsql.us-east-1.on.aws";
        let region = "us-east-1";
        let spec = PostgresConnectionSpec::AuroraDsql {
            endpoint: endpoint.to_string(),
            region: region.to_string(),
            admin: true,
        };
        assert!(matches!(
            spec,
            PostgresConnectionSpec::AuroraDsql {
                endpoint: ref ep,
                region: ref rg,
                admin: true,
            }
            if ep == endpoint && rg == region
        ));
    }
}
