use gqlforge_valid::{Valid, ValidationError, Validator};

use super::BlueprintError;
use crate::core::config::{Link, LinkType};
use crate::core::directive::DirectiveCodec;

#[derive(Debug)]
pub struct Links;

impl TryFrom<Vec<Link>> for Links {
    type Error = ValidationError<crate::core::blueprint::BlueprintError>;

    fn try_from(links: Vec<Link>) -> Result<Self, Self::Error> {
        Valid::from_iter(links.iter().enumerate(), |(pos, link)| {
            Valid::succeed(link.to_owned())
                .and_then(|link| {
                    if link.src.is_empty() {
                        Valid::fail(BlueprintError::LinkSrcCannotBeEmpty)
                    } else {
                        Valid::succeed(link)
                    }
                })
                .and_then(|link| {
                    if let Some(id) = &link.id
                        && links.iter().filter(|l| l.id.as_ref() == Some(id)).count() > 1
                    {
                        return Valid::fail(BlueprintError::Duplicated(id.clone()));
                    }
                    Valid::succeed(link)
                })
                .trace(&pos.to_string())
        })
        .and_then(|links| {
            let script_links = links
                .iter()
                .filter(|l| l.type_of == LinkType::Script)
                .collect::<Vec<&Link>>();

            if script_links.len() > 1 {
                Valid::fail(BlueprintError::OnlyOneScriptLinkAllowed)
            } else {
                Valid::succeed(links)
            }
        })
        .and_then(|links| {
            let key_links = links
                .iter()
                .filter(|l| l.type_of == LinkType::Key)
                .collect::<Vec<&Link>>();

            if key_links.len() > 1 {
                Valid::fail(BlueprintError::OnlyOneKeyLinkAllowed)
            } else {
                Valid::succeed(links)
            }
        })
        .and_then(|links| {
            let mut connection_ids = std::collections::HashSet::new();
            let collision = links
                .iter()
                .filter(|link| {
                    matches!(
                        link.type_of,
                        LinkType::Postgres | LinkType::GreptimeDb | LinkType::AuroraDsql
                    )
                })
                .find_map(|link| {
                    let id = link.id.as_deref().unwrap_or("default");
                    (!connection_ids.insert(id)).then(|| id.to_string())
                });

            if let Some(id) = collision {
                Valid::fail(BlueprintError::PostgresConnectionIdCollision(id))
            } else {
                Valid::succeed(links)
            }
        })
        .and_then(|links| {
            let redis_links: Vec<&Link> = links
                .iter()
                .filter(|l| l.type_of == LinkType::Redis)
                .collect();

            if redis_links.len() > 1 && redis_links.iter().any(|l| l.id.is_none()) {
                Valid::fail(BlueprintError::RedisMultipleLinksRequireId)
            } else {
                Valid::succeed(links)
            }
        })
        .trace(Link::trace_name().as_str())
        .trace("schema")
        .map_to(Links)
        .to_result()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "test code")]
    use super::*;

    fn database_link(type_of: LinkType, id: Option<&str>, src: &str) -> Link {
        Link {
            src: src.to_string(),
            type_of,
            id: id.map(std::string::ToString::to_string),
            ..Default::default()
        }
    }

    fn pg_link(id: Option<&str>, src: &str) -> Link {
        database_link(LinkType::Postgres, id, src)
    }

    fn redis_link(id: Option<&str>, src: &str) -> Link {
        Link {
            src: src.to_string(),
            type_of: LinkType::Redis,
            id: id.map(std::string::ToString::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn single_postgres_link_without_id_succeeds() {
        let links = vec![pg_link(None, "postgres://localhost/db")];
        assert!(Links::try_from(links).is_ok());
    }

    #[test]
    fn multiple_postgres_links_with_ids_succeeds() {
        let links = vec![
            pg_link(Some("main"), "postgres://localhost/main"),
            pg_link(Some("analytics"), "postgres://localhost/analytics"),
        ];
        assert!(Links::try_from(links).is_ok());
    }

    #[test]
    fn named_and_unnamed_postgres_links_succeed() {
        let links = vec![
            pg_link(Some("main"), "postgres://localhost/main"),
            pg_link(None, "postgres://localhost/analytics"),
        ];

        assert!(Links::try_from(links).is_ok());
    }

    #[test]
    fn multiple_postgres_links_all_missing_id_fails() {
        let links = vec![
            pg_link(None, "postgres://localhost/main"),
            pg_link(None, "postgres://localhost/analytics"),
        ];
        let result = Links::try_from(links);
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_postgres_link_ids_fails() {
        let links = vec![
            pg_link(Some("main"), "postgres://localhost/db1"),
            pg_link(Some("main"), "postgres://localhost/db2"),
        ];
        let result = Links::try_from(links);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let messages: Vec<String> = err.as_vec().iter().map(|c| c.message.to_string()).collect();
        assert!(
            messages.iter().any(|m| m.contains("Duplicated")),
            "Expected Duplicated error, got: {messages:?}"
        );
    }

    #[test]
    fn postgres_and_named_greptimedb_links_succeed() {
        let links = vec![
            pg_link(None, "postgres://localhost/app"),
            database_link(
                LinkType::GreptimeDb,
                Some("metrics"),
                "postgres://localhost:4003/public",
            ),
        ];

        assert!(Links::try_from(links).is_ok());
    }

    #[test]
    fn unnamed_greptimedb_and_aurora_dsql_links_fail() {
        let links = vec![
            database_link(
                LinkType::GreptimeDb,
                None,
                "postgres://localhost:4003/public",
            ),
            database_link(LinkType::AuroraDsql, None, "cluster.dsql.us-east-1.on.aws"),
        ];

        let error = Links::try_from(links).unwrap_err();
        assert!(error.as_vec().iter().any(|cause| {
            cause
                .message
                .to_string()
                .contains("connection id 'default'")
        }));
    }

    #[test]
    fn single_redis_link_without_id_succeeds() {
        let links = vec![redis_link(None, "redis://localhost:6379")];
        assert!(Links::try_from(links).is_ok());
    }

    #[test]
    fn multiple_redis_links_with_ids_succeeds() {
        let links = vec![
            redis_link(Some("main"), "redis://localhost:6379/0"),
            redis_link(Some("cache"), "redis://localhost:6379/1"),
        ];
        assert!(Links::try_from(links).is_ok());
    }

    #[test]
    fn multiple_redis_links_missing_id_fails() {
        let links = vec![
            redis_link(Some("main"), "redis://localhost:6379/0"),
            redis_link(None, "redis://localhost:6379/1"),
        ];
        let result = Links::try_from(links);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let messages: Vec<String> = err.as_vec().iter().map(|c| c.message.to_string()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Multiple @link(type: Redis)")),
            "Expected RedisMultipleLinksRequireId error, got: {messages:?}"
        );
    }

    #[test]
    fn multiple_redis_links_all_missing_id_fails() {
        let links = vec![
            redis_link(None, "redis://localhost:6379/0"),
            redis_link(None, "redis://localhost:6379/1"),
        ];
        let result = Links::try_from(links);
        assert!(result.is_err());
    }
}
