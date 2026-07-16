use gqlforge_macros::DirectiveDefinition;
use serde::{Deserialize, Serialize};

use crate::core::config::KeyValue;
use crate::core::is_default;

#[derive(
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Debug,
    Clone,
    schemars::JsonSchema,
    strum_macros::Display,
)]
pub enum LinkType {
    #[default]
    /// Points to another Gqlforge Configuration file. The imported
    /// configuration will be merged into the importing configuration.
    Config,

    /// Points to a Protobuf file. The imported Protobuf file will be used by
    /// the `@grpc` directive. If your API exposes a reflection endpoint, you
    /// should set the type to `Grpc` instead.
    Protobuf,

    /// Points to a JS file. The imported JS file will be used by the `@js`
    /// directive.
    Script,

    /// Points to a Cert file. The imported Cert file will be used by the server
    /// to serve over HTTPS.
    Cert,

    /// Points to a Key file. The imported Key file will be used by the server
    /// to serve over HTTPS.
    Key,

    /// A trusted document that contains GraphQL operations (queries, mutations)
    /// that can be exposed a REST API using the `@rest` directive.
    Operation,

    /// Points to a Htpasswd file. The imported Htpasswd file will be used by
    /// the server to authenticate users.
    Htpasswd,

    /// Points to a Jwks file. The imported Jwks file will be used by the server
    /// to authenticate users.
    Jwks,

    /// Points to a reflection endpoint. The imported reflection endpoint will
    /// be used by the `@grpc` directive to resolve data from gRPC services.
    Grpc,

    /// Points to a SQL migration file. Used to build a database schema for
    /// the `@postgres` and `@greptimedb` directives offline (without a live
    /// database connection).
    Sql,

    /// Points to a `PostgreSQL` connection string. The database will be
    /// introspected at startup to build a schema for the `@postgres` directive.
    Postgres,

    /// Points to a `GreptimeDB` connection string using its
    /// `PostgreSQL`-compatible protocol. The database is introspected at
    /// startup to build a schema for the `@greptimedb` directive.
    #[serde(rename = "GreptimeDB")]
    #[strum(to_string = "GreptimeDB")]
    GreptimeDb,

    /// Points to an S3 or S3-compatible endpoint. The endpoint URL and
    /// region/credentials metadata are used by the `@s3` directive.
    S3,

    /// Points to an Aurora DSQL cluster endpoint. The cluster will be
    /// introspected at startup using AWS IAM authentication.
    /// `src` should be the cluster endpoint (without scheme).
    /// `meta.region` is required; `meta.admin` defaults to false.
    AuroraDsql,

    /// Points to a Redis connection URL. Used by the `@redis` directive.
    Redis,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aurora_dsql_link_type_display() {
        assert_eq!(LinkType::AuroraDsql.to_string(), "AuroraDsql");
    }

    #[test]
    fn aurora_dsql_link_type_serializes_to_json() -> serde_json::Result<()> {
        let link = Link {
            type_of: LinkType::AuroraDsql,
            src: "cluster123.dsql.us-east-1.on.aws".to_string(),
            meta: Some(serde_json::json!({ "region": "us-east-1", "admin": true })),
            ..Default::default()
        };
        let json = serde_json::to_value(&link)?;
        assert_eq!(json["type"], "AuroraDsql");
        assert_eq!(json["src"], "cluster123.dsql.us-east-1.on.aws");
        Ok(())
    }

    #[test]
    fn aurora_dsql_link_type_deserializes_from_json() -> serde_json::Result<()> {
        let json = r#"{"src":"cluster.dsql.us-east-1.on.aws","type":"AuroraDsql","meta":{"region":"us-east-1"}}"#;
        let link: Link = serde_json::from_str(json)?;
        assert_eq!(link.type_of, LinkType::AuroraDsql);
        assert_eq!(link.src, "cluster.dsql.us-east-1.on.aws");
        Ok(())
    }

    #[test]
    fn greptimedb_link_type_uses_its_public_name() -> serde_json::Result<()> {
        let link = Link {
            type_of: LinkType::GreptimeDb,
            src: "postgresql://greptime@localhost:4003/public".to_string(),
            ..Default::default()
        };

        assert_eq!(LinkType::GreptimeDb.to_string(), "GreptimeDB");
        assert_eq!(serde_json::to_value(&link)?["type"], "GreptimeDB");
        assert_eq!(
            serde_json::from_value::<Link>(serde_json::json!({
                "src": "postgresql://greptime@localhost:4003/public",
                "type": "GreptimeDB"
            }))?
            .type_of,
            LinkType::GreptimeDb
        );
        Ok(())
    }

    #[test]
    fn aurora_dsql_meta_region_extracted() {
        let link = Link {
            type_of: LinkType::AuroraDsql,
            src: "cluster.dsql.ap-northeast-1.on.aws".to_string(),
            meta: Some(serde_json::json!({ "region": "ap-northeast-1" })),
            ..Default::default()
        };
        assert_eq!(link.dsql_region(), Some("ap-northeast-1"));
    }

    #[test]
    fn aurora_dsql_meta_admin_defaults_to_false() {
        let link = Link {
            type_of: LinkType::AuroraDsql,
            src: "cluster.dsql.us-east-1.on.aws".to_string(),
            meta: Some(serde_json::json!({ "region": "us-east-1" })),
            ..Default::default()
        };
        assert!(!link.dsql_admin());
    }

    #[test]
    fn aurora_dsql_meta_admin_can_be_true() {
        let link = Link {
            type_of: LinkType::AuroraDsql,
            src: "cluster.dsql.us-east-1.on.aws".to_string(),
            meta: Some(serde_json::json!({ "region": "us-east-1", "admin": true })),
            ..Default::default()
        };
        assert!(link.dsql_admin());
    }
}

impl Link {
    const REGION_KEY: &str = "region";
    const ADMIN_KEY: &str = "admin";

    #[must_use]
    pub fn dsql_region(&self) -> Option<&str> {
        self.meta
            .as_ref()
            .and_then(|m| m.get(Self::REGION_KEY))
            .and_then(|v| v.as_str())
    }

    pub fn dsql_admin(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|m| m.get(Self::ADMIN_KEY))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

/// The @link directive allows you to import external resources, such as
/// configuration - which will be merged into the config importing it -,
/// or a .proto file - which will be later used by the `@grpc` directive.
#[derive(
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Debug,
    Clone,
    schemars::JsonSchema,
    DirectiveDefinition,
)]
#[directive_definition(repeatable, locations = "Schema")]
#[serde(deny_unknown_fields)]
pub struct Link {
    ///
    /// The id of the link. It is used to reference the link in the schema.
    #[serde(default, skip_serializing_if = "is_default")]
    pub id: Option<String>,
    ///
    /// The source of the link. It can be a URL or a path to a file.
    /// If a path is provided, it is relative to the file that imports the link.
    #[serde(default, skip_serializing_if = "is_default")]
    pub src: String,
    ///
    /// The type of the link. It can be `Config`, or `Protobuf`.
    #[serde(default, skip_serializing_if = "is_default", rename = "type")]
    pub type_of: LinkType,
    ///
    /// Custom headers for gRPC reflection server.
    #[serde(default, skip_serializing_if = "is_default")]
    pub headers: Option<Vec<KeyValue>>,
    ///
    /// Additional metadata pertaining to the linked resource.
    #[serde(default, skip_serializing_if = "is_default")]
    pub meta: Option<serde_json::Value>,
    ///
    /// The proto paths to be used when resolving dependencies.
    /// Only valid when [`Link::type_of`] is [`LinkType::Protobuf`]
    #[serde(default, skip_serializing_if = "is_default")]
    pub proto_paths: Option<Vec<String>>,
}
