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
            let pg_links: Vec<&Link> = links
                .iter()
                .filter(|l| l.type_of == LinkType::Postgres)
                .collect();

            if pg_links.len() > 1 && pg_links.iter().any(|l| l.id.is_none()) {
                Valid::fail(BlueprintError::PostgresMultipleLinksRequireId)
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

    fn pg_link(id: Option<&str>, src: &str) -> Link {
        Link {
            src: src.to_string(),
            type_of: LinkType::Postgres,
            id: id.map(std::string::ToString::to_string),
            ..Default::default()
        }
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
    fn multiple_postgres_links_missing_id_fails() {
        let links = vec![
            pg_link(Some("main"), "postgres://localhost/main"),
            pg_link(None, "postgres://localhost/analytics"),
        ];
        let result = Links::try_from(links);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let messages: Vec<String> = err.as_vec().iter().map(|c| c.message.to_string()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Multiple @link(type: Postgres)")),
            "Expected PostgresMultipleLinksRequireId error, got: {messages:?}"
        );
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
