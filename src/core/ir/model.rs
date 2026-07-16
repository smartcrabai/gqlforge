use std::collections::HashMap;
use std::fmt::Debug;
use std::num::NonZeroU64;

use gqlrs::Value;
use strum_macros::Display;

use super::access_expr::AccessExpr;
use super::discriminator::Discriminator;
use super::{EvalContext, ResolverContextLike};
use crate::core::blueprint::{Auth, DynamicValue};
use crate::core::config::PostgresPayloadType;
use crate::core::config::group_by::GroupBy;
use crate::core::graphql::{self};
use crate::core::redis::{RedisPayloadType, RedisStreamSource};
use crate::core::worker_hooks::WorkerHooks;
use crate::core::{grpc, http, postgres, redis, s3};

#[derive(Clone, Debug, Display)]
pub enum IR {
    Dynamic(DynamicValue<Value>),
    #[strum(to_string = "{0}")]
    IO(Box<IO>),
    Cache(Cache),
    // TODO: Path can be implement using Pipe
    Path(Box<IR>, Vec<String>),
    ContextPath(Vec<String>),
    Protect(Auth, Option<AccessExpr>, Box<IR>),
    Map(Map),
    Pipe(Box<IR>, Box<IR>),
    /// Merges the result of multiple IRs together
    Merge(Vec<IR>),
    Discriminate(Discriminator, Box<IR>),
    /// Apollo Federation _entities resolver
    Entity(HashMap<String, IR>),
    /// Apollo Federation _service resolver
    Service(String),
}

#[derive(Clone, Debug)]
pub struct Map {
    pub input: Box<IR>,
    // accept key return value instead of
    pub map: HashMap<String, String>,
}

#[derive(Clone, Debug, strum_macros::Display)]
pub enum IO {
    Http {
        req_template: Box<http::RequestTemplate>,
        group_by: Option<GroupBy>,
        dl_id: Option<DataLoaderId>,
        is_list: bool,
        dedupe: bool,
        hook: Option<WorkerHooks>,
    },
    GraphQL {
        req_template: graphql::RequestTemplate,
        field_name: String,
        batch: bool,
        dl_id: Option<DataLoaderId>,
        dedupe: bool,
    },
    Grpc {
        req_template: grpc::RequestTemplate,
        group_by: Option<GroupBy>,
        dl_id: Option<DataLoaderId>,
        dedupe: bool,
        hook: Option<WorkerHooks>,
    },
    GrpcStream {
        req_template: grpc::RequestTemplate,
        hook: Option<WorkerHooks>,
    },
    GraphQLStream {
        req_template: graphql::RequestTemplate,
        field_name: String,
        stream_url: String,
    },
    HttpStream {
        req_template: Box<http::RequestTemplate>,
        hook: Option<WorkerHooks>,
    },
    Js {
        name: String,
    },
    Postgres {
        req_template: postgres::RequestTemplate,
        group_by: Option<GroupBy>,
        dl_id: Option<DataLoaderId>,
        dedupe: bool,
        connection_id: String,
    },
    PostgresStream {
        connection_id: String,
        channel: String,
        payload_type: PostgresPayloadType,
    },
    Redis {
        req_template: redis::RequestTemplate,
        dedupe: bool,
        connection_id: String,
    },
    RedisStream {
        connection_id: String,
        source: RedisStreamSource,
        payload_type: RedisPayloadType,
    },
    S3 {
        req_template: s3::RequestTemplate,
        dedupe: bool,
    },
}

impl IO {
    #[must_use]
    pub fn dedupe(&self) -> bool {
        match self {
            IO::Http { dedupe, .. }
            | IO::GraphQL { dedupe, .. }
            | IO::Grpc { dedupe, .. }
            | IO::Postgres { dedupe, .. }
            | IO::Redis { dedupe, .. }
            | IO::S3 { dedupe, .. } => *dedupe,
            IO::GrpcStream { .. }
            | IO::GraphQLStream { .. }
            | IO::HttpStream { .. }
            | IO::PostgresStream { .. }
            | IO::RedisStream { .. }
            | IO::Js { .. } => false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DataLoaderId(usize);

impl DataLoaderId {
    #[must_use]
    pub fn new(id: usize) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_usize(&self) -> usize {
        self.0
    }
}

#[derive(PartialEq, Eq, Clone, Hash, Debug)]
pub struct IoId(u64);

impl IoId {
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

pub trait CacheKey<Ctx> {
    fn cache_key(&self, ctx: &Ctx) -> Option<IoId>;
}

#[derive(Clone, Debug)]
pub struct Cache {
    pub max_age: NonZeroU64,
    pub io: Box<IO>,
}

impl Cache {
    ///
    /// Wraps an expression with the cache primitive.
    /// Performance DFS on the cache on the expression and identifies all the IO
    /// nodes. Then wraps each IO node with the cache primitive.
    pub fn wrap(max_age: NonZeroU64, expr: IR) -> IR {
        expr.modify(&mut move |expr| match expr {
            IR::IO(io) => Some(IR::Cache(Cache { max_age, io: io.to_owned() })),
            _ => None,
        })
    }
}

impl IR {
    // allows to modify the IO node in the IR tree
    pub fn modify_io(&mut self, io_modifier: &mut dyn FnMut(&mut IO)) {
        match self {
            IR::IO(io) => io_modifier(io),
            IR::Cache(cache) => io_modifier(&mut cache.io),
            IR::Discriminate(_, ir) | IR::Protect(_, _, ir) | IR::Path(ir, _) => {
                ir.modify_io(io_modifier);
            }
            IR::Pipe(ir1, ir2) => {
                ir1.modify_io(io_modifier);
                ir2.modify_io(io_modifier);
            }
            IR::Entity(hash_map) => {
                for ir in hash_map.values_mut() {
                    ir.modify_io(io_modifier);
                }
            }
            IR::Map(map) => map.input.modify_io(io_modifier),
            _ => {}
        }
    }

    #[must_use]
    pub fn pipe(self, next: Self) -> Self {
        IR::Pipe(Box::new(self), Box::new(next))
    }

    #[must_use]
    pub fn modify<F: FnMut(&IR) -> Option<IR>>(self, modifier: &mut F) -> IR {
        self.modify_inner(modifier)
    }

    fn modify_box<F: FnMut(&IR) -> Option<IR>>(self, modifier: &mut F) -> Box<IR> {
        Box::new(self.modify_inner(modifier))
    }

    fn modify_inner<F: FnMut(&IR) -> Option<IR>>(self, modifier: &mut F) -> IR {
        let replacement = modifier(&self);
        if let Some(expr) = replacement {
            expr
        } else {
            let expr = self;
            match expr {
                IR::Pipe(first, second) => {
                    IR::Pipe(first.modify_box(modifier), second.modify_box(modifier))
                }
                IR::ContextPath(path) => IR::ContextPath(path),
                IR::Dynamic(_) | IR::IO(_) => expr,
                IR::Cache(Cache { io, max_age }) => {
                    let expr = *IR::IO(io).modify_box(modifier);
                    match expr {
                        IR::IO(io) => IR::Cache(Cache { max_age, io }),
                        expr => expr,
                    }
                }
                IR::Path(expr, path) => IR::Path(expr.modify_box(modifier), path),
                IR::Protect(auth, access_expr, expr) => {
                    IR::Protect(auth, access_expr, expr.modify_box(modifier))
                }
                IR::Map(Map { input, map }) => {
                    IR::Map(Map { input: input.modify_box(modifier), map })
                }
                IR::Discriminate(discriminator, expr) => {
                    IR::Discriminate(discriminator, expr.modify_box(modifier))
                }
                IR::Entity(map) => IR::Entity(
                    map.into_iter()
                        .map(|(k, v)| (k, v.modify(modifier)))
                        .collect(),
                ),
                IR::Service(sdl) => IR::Service(sdl),
                IR::Merge(vec) => {
                    IR::Merge(vec.into_iter().map(|ir| ir.modify(modifier)).collect())
                }
            }
        }
    }
}

impl<'a, Ctx: ResolverContextLike + Sync> CacheKey<EvalContext<'a, Ctx>> for IO {
    fn cache_key(&self, ctx: &EvalContext<'a, Ctx>) -> Option<IoId> {
        match self {
            IO::Http { req_template, .. } => req_template.cache_key(ctx),
            IO::Grpc { req_template, .. } => req_template.cache_key(ctx),
            IO::GrpcStream { .. }
            | IO::GraphQLStream { .. }
            | IO::HttpStream { .. }
            | IO::PostgresStream { .. }
            | IO::RedisStream { .. }
            | IO::Js { .. } => None,
            IO::GraphQL { req_template, .. } => req_template.cache_key(ctx),
            IO::Postgres { req_template, connection_id, .. } => {
                req_template.cache_key_with_connection(ctx, connection_id)
            }
            IO::Redis { req_template, connection_id, .. } => {
                req_template.cache_key_with_connection(ctx, connection_id)
            }
            IO::S3 { req_template, .. } => req_template.cache_key(ctx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_stream_dedupe_returns_false() {
        let io = IO::PostgresStream {
            connection_id: "main".to_string(),
            channel: "users_changes".to_string(),
            payload_type: PostgresPayloadType::Json,
        };
        assert!(!io.dedupe());
    }

    #[test]
    fn redis_stream_dedupe_returns_false() {
        let io = IO::RedisStream {
            connection_id: "main".to_string(),
            source: RedisStreamSource::PubSub {
                channel: crate::core::mustache::Mustache::parse("events"),
            },
            payload_type: RedisPayloadType::Json,
        };
        assert!(!io.dedupe());
    }
}
