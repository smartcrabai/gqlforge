use std::collections::{BTreeMap, BTreeSet};

use async_graphql::dynamic::SchemaBuilder;
use gqlforge_valid::{Valid, ValidationError, Validator};
use indexmap::IndexMap;

use self::telemetry::to_opentelemetry;
use super::Server;
use crate::core::Type;
use crate::core::blueprint::PostgresConnectionSpec;
use crate::core::blueprint::compress::compress;
use crate::core::blueprint::{
    Blueprint, BlueprintError, Definition, Links, TryFoldConfig, Upstream, telemetry,
    to_definitions, to_schema, update_federation,
};
use crate::core::config::transformer::Required;
use crate::core::config::{Arg, Batch, Config, ConfigModule};
use crate::core::ir::model::{IO, IR};
use crate::core::json::JsonSchema;
use crate::core::try_fold::TryFold;

/// Maps a single `Postgres` or `AuroraDsql` link to a `(id, PostgresConnectionSpec)` pair.
fn link_to_connection_spec(
    link: &crate::core::config::Link,
) -> anyhow::Result<(String, PostgresConnectionSpec)> {
    let id = link.id.clone().unwrap_or_else(|| "default".to_string());
    let spec = match link.type_of {
        crate::core::config::LinkType::Postgres => PostgresConnectionSpec::Url(link.src.clone()),
        crate::core::config::LinkType::AuroraDsql => {
            let region = link
                .dsql_region()
                .ok_or_else(|| anyhow::anyhow!("AuroraDsql link requires meta.region"))?
                .to_string();
            let admin = link.dsql_admin();
            PostgresConnectionSpec::AuroraDsql { endpoint: link.src.clone(), region, admin }
        }
        _ => unreachable!("caller must filter to Postgres/AuroraDsql only"),
    };
    Ok((id, spec))
}

pub fn config_blueprint<'a>() -> TryFold<'a, ConfigModule, Blueprint, BlueprintError> {
    let server = TryFoldConfig::<Blueprint>::new(|config_module, blueprint| {
        Valid::from(Server::try_from(config_module.clone())).map(|server| blueprint.server(server))
    });

    let schema = to_schema().transform::<Blueprint>(
        |schema, blueprint| blueprint.schema(schema),
        |blueprint| blueprint.schema,
    );

    let definitions = to_definitions().transform::<Blueprint>(
        |definitions, blueprint| blueprint.definitions(definitions),
        |blueprint| blueprint.definitions,
    );

    let upstream = TryFoldConfig::<Blueprint>::new(|config_module, blueprint| {
        Valid::from(Upstream::try_from(config_module)).map(|upstream| blueprint.upstream(upstream))
    });

    let links = TryFoldConfig::<Blueprint>::new(|config_module, blueprint| {
        Valid::from(Links::try_from(config_module.links.clone())).map_to(blueprint)
    });

    let opentelemetry = to_opentelemetry().transform::<Blueprint>(
        |opentelemetry, blueprint| blueprint.telemetry(opentelemetry),
        |blueprint| blueprint.telemetry,
    );

    let postgres_connections = TryFoldConfig::<Blueprint>::new(|config_module, mut blueprint| {
        let connections: Result<Vec<_>, _> = config_module
            .links
            .iter()
            .filter(|link| {
                link.type_of == crate::core::config::LinkType::Postgres
                    || link.type_of == crate::core::config::LinkType::AuroraDsql
            })
            .map(link_to_connection_spec)
            .collect();
        match connections {
            Ok(connections) => {
                blueprint.postgres_connections = connections;
                Valid::succeed(blueprint)
            }
            Err(e) => Valid::fail(BlueprintError::Error(e)),
        }
    });

    server
        .and(schema)
        .and(definitions)
        .and(upstream)
        .and(links)
        .and(opentelemetry)
        .and(postgres_connections)
        // set the federation config only after setting other properties to be able
        // to use blueprint inside the handler and to avoid recursion overflow
        .and(update_federation().trace("federation"))
        .update(apply_batching)
        .update(compress)
}

// Apply batching if any of the fields have a @http directive with groupBy field

pub fn apply_batching(mut blueprint: Blueprint) -> Blueprint {
    for def in &blueprint.definitions {
        if let Definition::Object(object_type_definition) = def {
            for field in &object_type_definition.fields {
                if let Some(IR::IO(io)) = field.resolver.as_ref()
                    && matches!(io.as_ref(), IO::Http { group_by: Some(_), .. })
                {
                    blueprint.upstream.batch = blueprint.upstream.batch.or(Some(Batch::default()));
                    return blueprint;
                }
            }
        }
    }
    blueprint
}

#[must_use]
pub fn to_json_schema_for_args(args: &IndexMap<String, Arg>, config: &Config) -> JsonSchema {
    let mut schema_fields = BTreeMap::new();
    for (name, arg) in args {
        schema_fields.insert(name.clone(), to_json_schema(&arg.type_of, config));
    }
    JsonSchema::Obj(schema_fields)
}
#[must_use]
pub fn to_json_schema(type_of: &Type, config: &Config) -> JsonSchema {
    let json_schema = match type_of {
        Type::Named { name, .. } => {
            let type_ = config.find_type(name);
            let type_enum_ = config.find_enum(name);

            if let Some(type_) = type_ {
                let mut schema_fields = BTreeMap::new();
                for (name, field) in &type_.fields {
                    if field.resolvers.is_empty() {
                        schema_fields.insert(name.clone(), to_json_schema(&field.type_of, config));
                    }
                }
                JsonSchema::Obj(schema_fields)
            } else if let Some(type_enum_) = type_enum_ {
                JsonSchema::Enum(
                    type_enum_
                        .variants
                        .iter()
                        .map(|variant| variant.name.clone())
                        .collect::<BTreeSet<String>>(),
                )
            } else {
                JsonSchema::from_scalar_type(name)
            }
        }
        Type::List { of_type, .. } => JsonSchema::Arr(Box::new(to_json_schema(of_type, config))),
    };

    if type_of.is_nullable() {
        JsonSchema::Opt(Box::new(json_schema))
    } else {
        json_schema
    }
}

impl TryFrom<&ConfigModule> for Blueprint {
    type Error = ValidationError<crate::core::blueprint::BlueprintError>;

    fn try_from(config_module: &ConfigModule) -> Result<Self, Self::Error> {
        config_blueprint()
            .try_fold(
                // Apply required transformers to the configuration
                &config_module
                    .to_owned()
                    .transform(&Required)
                    .to_result()
                    .map_err(|e| BlueprintError::from_validation_string(&e))?,
                Blueprint::default(),
            )
            .and_then(|blueprint| {
                let schema_builder = SchemaBuilder::from(&blueprint);
                match schema_builder.finish() {
                    Ok(_) => Valid::succeed(blueprint),
                    Err(e) => Valid::fail(e.into()),
                }
            })
            .to_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Link, LinkType};

    fn make_link(
        type_of: LinkType,
        src: &str,
        id: Option<&str>,
        meta: Option<serde_json::Value>,
    ) -> Link {
        Link {
            type_of,
            src: src.to_string(),
            id: id.map(str::to_string),
            meta,
            ..Default::default()
        }
    }

    #[test]
    fn postgres_link_maps_to_url_spec() -> anyhow::Result<()> {
        let link = make_link(
            LinkType::Postgres,
            "postgresql://user:pass@host/db",
            None,
            None,
        );
        let (id, spec) = link_to_connection_spec(&link)?;
        assert_eq!(id, "default");
        assert!(
            matches!(spec, PostgresConnectionSpec::Url(u) if u == "postgresql://user:pass@host/db")
        );
        Ok(())
    }

    #[test]
    fn postgres_link_with_explicit_id() -> anyhow::Result<()> {
        let link = make_link(
            LinkType::Postgres,
            "postgresql://host/db",
            Some("main"),
            None,
        );
        let (id, _spec) = link_to_connection_spec(&link)?;
        assert_eq!(id, "main");
        Ok(())
    }

    #[test]
    fn aurora_dsql_link_maps_to_dsql_spec() -> anyhow::Result<()> {
        let link = make_link(
            LinkType::AuroraDsql,
            "cluster.dsql.us-east-1.on.aws",
            None,
            Some(serde_json::json!({ "region": "us-east-1" })),
        );
        let (id, spec) = link_to_connection_spec(&link)?;
        assert_eq!(id, "default");
        assert!(matches!(
            spec,
            PostgresConnectionSpec::AuroraDsql {
                ref endpoint,
                ref region,
                admin: false,
            }
            if endpoint == "cluster.dsql.us-east-1.on.aws" && region == "us-east-1"
        ));
        Ok(())
    }

    #[test]
    fn aurora_dsql_link_with_admin_true() -> anyhow::Result<()> {
        let link = make_link(
            LinkType::AuroraDsql,
            "cluster.dsql.us-east-1.on.aws",
            Some("dsql_admin"),
            Some(serde_json::json!({ "region": "us-east-1", "admin": true })),
        );
        let (id, spec) = link_to_connection_spec(&link)?;
        assert_eq!(id, "dsql_admin");
        assert!(matches!(
            spec,
            PostgresConnectionSpec::AuroraDsql { admin: true, .. }
        ));
        Ok(())
    }

    #[test]
    fn aurora_dsql_link_missing_region_returns_error() {
        let link = make_link(
            LinkType::AuroraDsql,
            "cluster.dsql.eu-west-1.on.aws",
            None,
            Some(serde_json::json!({})),
        );
        let Err(e) = link_to_connection_spec(&link) else {
            panic!("expected error for missing region");
        };
        assert!(e.to_string().contains("meta.region"));
    }

    #[test]
    fn aurora_dsql_link_null_meta_returns_error() {
        let link = make_link(
            LinkType::AuroraDsql,
            "cluster.dsql.us-east-1.on.aws",
            None,
            None,
        );
        let Err(e) = link_to_connection_spec(&link) else {
            panic!("expected error for null meta");
        };
        assert!(e.to_string().contains("meta.region"));
    }
}
