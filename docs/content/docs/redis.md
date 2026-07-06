+++
title = "Redis Support"
description = "Expose Redis commands, Pub/Sub, and Streams as a GraphQL API."
+++

# Redis Support

GQLForge can resolve GraphQL fields directly against a Redis (or Redis-compatible) server using the `@redis` directive — including key-value, hash, list, and set commands, Pub/Sub messaging, and Streams.

## Connecting to Redis

Register a connection with `@link` using the `Redis` link type:

```graphql
schema @server(port: 8000) @link(type: Redis, src: "redis://localhost:6379") {
  query: Query
  mutation: Mutation
}
```

Use the `rediss://` scheme instead of `redis://` to connect over TLS:

```graphql
schema @server(port: 8000) @link(type: Redis, src: "rediss://user:password@redis.example.com:6380") {
  query: Query
}
```

## Multiple Redis Connections

You can connect to multiple Redis servers by defining multiple `@link(type: Redis)` directives. Each link must have a unique `id`:

```graphql
schema
@server(port: 8000)
@link(id: "cache", type: Redis, src: "redis://localhost:6379")
@link(id: "pubsub", type: Redis, src: "rediss://pubsub.internal:6380") {
  query: Query
  mutation: Mutation
  subscription: Subscription
}
```

Use the `db` field on `@redis` to specify which connection a field should use:

```graphql
type Query {
  cachedValue(key: String!): String @redis(db: "cache", key: "cache:{{.args.key}}")
}
```

When only a single `@link(type: Redis)` is defined, `db` can be omitted — the field automatically resolves to that connection. If two or more Redis links exist and `db` is omitted, the schema fails to compile.

## The @redis Directive

Use `@redis` on fields to map them to Redis commands:

```graphql
type Query {
  cachedValue(key: String!): String @redis(key: "cache:{{.args.key}}")

  userProfile(id: ID!): JSON @redis(operation: HGETALL, key: "user:{{.args.id}}")

  recentEvents(stop: Int): [String!]! @redis(operation: LRANGE, key: "events:recent", stop: "{{.args.stop}}")
}

type Mutation {
  cacheValue(key: String!, value: String!, ttlSeconds: Int): Boolean
  @redis(operation: SET, key: "cache:{{.args.key}}", value: "{{.args.value}}", ttl: "{{.args.ttlSeconds}}")

  setUserField(id: ID!, field: String!, value: String!): Boolean
  @redis(operation: HSET, key: "user:{{.args.id}}", field: "{{.args.field}}", value: "{{.args.value}}")
}
```

`key`, `field`, `value`, `ttl`, `start`, `stop`, and `channel` all support Mustache templates such as `{{.args.id}}` (field arguments) or `{{.value.id}}` (parent object, when resolving a nested field).

See [@redis Directive](@/docs/directives/redis.md) for the full field reference, the required-field table per `RedisOperation`, and the `XADD` payload-expansion rules.

## Payload Decoding (`payloadType`)

Redis stores everything as strings (or arrays/maps of strings). `payloadType` controls how those raw strings are turned into GraphQL values:

- **`JSON`** (default) — try to parse the string as JSON; if it isn't valid JSON, fall back to returning it as a plain string. This lets you store either structured JSON documents or plain scalars under the same key without extra bookkeeping.
- **`TEXT`** — always return the raw string, even if it happens to look like JSON.

Decoding is applied to every string leaf of the result, so a `HGETALL` map or an `XREAD` entry's `values` object gets each of its values decoded independently:

```graphql
type Query {
  rawSessionToken(id: ID!): String @redis(key: "session:{{.args.id}}", payloadType: TEXT)
}
```

Because `JSON` parses _every_ string leaf, a stored value that merely looks like a number, boolean, or JSON structure is decoded into that type rather than kept as a literal string — `SET cache:1 42` makes `cachedValue` resolve to the number `42`, and `SET cache:flag true` resolves to the boolean `true`. If you need the exact stored string back (e.g. a token, a hash, or a value that happens to be all digits), set `payloadType: TEXT` on that field:

```graphql
type Query {
  # "42" is stored, but payloadType: JSON (the default) decodes it to the number 42.
  looksLikeANumber(key: String!): JSON @redis(key: "cache:{{.args.key}}")

  # payloadType: TEXT always returns the literal string "42".
  rawValue(key: String!): String @redis(key: "cache:{{.args.key}}", payloadType: TEXT)
}
```

`SET` and `EXISTS` results are normalized to `Boolean` regardless of `payloadType` (see the [`RedisOperation` table](@/docs/directives/redis.md#redisoperation) for details) — `payloadType` only affects how _string_ leaves inside the result are interpreted, not these two operations' own return values.

## Pub/Sub vs. Streams

`@redis` exposes two ways to push Redis data to GraphQL subscribers, available only on `Subscription` fields:

|                    | `SUBSCRIBE` (Pub/Sub)                                                              | `XREAD` (Streams)                                                          |
| ------------------ | ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| Backing primitive  | Redis Pub/Sub channel                                                              | Redis Stream (`XADD`/`XREAD`)                                              |
| Persistence        | Not persisted — a message only reaches clients that are connected at publish time. | Persisted as stream entries until trimmed/expired.                         |
| Delivery semantics | At-most-once. Disconnected clients silently miss messages.                         | At-least-once–leaning. Reconnects resume from the last delivered entry id. |
| Required field     | `channel`                                                                          | `key` (`startId` optional, defaults to `"$"`, i.e. only new entries)       |

Use `SUBSCRIBE` for lightweight, ephemeral notifications where an occasional missed message is acceptable (cache invalidation hints, live cursors, presence pings). Use `XREAD` against a Stream when subscribers need a durable, resumable feed (audit trails, event sourcing, activity feeds) — because Redis retains the entries, a client that briefly disconnects can catch back up.

```graphql
type Subscription {
  notifications(channel: String!): JSON @redis(operation: SUBSCRIBE, channel: "alerts:{{.args.channel}}")

  streamEvents(key: String!): JSON @redis(operation: XREAD, key: "stream:{{.args.key}}")
}
```

Subscriptions are delivered over SSE: `POST`-ing a subscription operation to the GraphQL endpoint returns a `text/event-stream` response, with one event per message (`SUBSCRIBE`) or stream entry (`XREAD`). See [@redis Directive — Subscriptions](@/docs/directives/redis.md#subscriptions) for the exact event shape and a `curl` example.

## Reconnection Behavior

If the connection to Redis drops, GQLForge automatically reconnects:

- **Pub/Sub (`SUBSCRIBE`)** re-subscribes to the channel on reconnect, but any messages published while disconnected are gone — Pub/Sub has no memory of past messages.
- **Streams (`XREAD`)** resumes from the last entry id it successfully delivered before the disconnect, rather than restarting from `startId`. This avoids re-delivering the same entries on every reconnect while still catching subscribers up on what they missed.

## Example: Full Schema

```graphql
schema @server(port: 8000) @link(type: Redis, src: "redis://localhost:6379") @upstream(httpCache: 42) {
  query: Query
  mutation: Mutation
  subscription: Subscription
}

type Query {
  cachedValue(key: String!): String @redis(key: "cache:{{.args.key}}")
  userProfile(id: ID!): JSON @redis(operation: HGETALL, key: "user:{{.args.id}}")
  tags: [String!]! @redis(operation: SMEMBERS, key: "tags:all")
}

type Mutation {
  cacheValue(key: String!, value: String!, ttlSeconds: Int): Boolean
  @redis(operation: SET, key: "cache:{{.args.key}}", value: "{{.args.value}}", ttl: "{{.args.ttlSeconds}}")

  addTag(tag: String!): Int @redis(operation: SADD, key: "tags:all", value: "{{.args.tag}}")

  publishAlert(channel: String!, message: String!): Int
  @redis(operation: PUBLISH, channel: "alerts:{{.args.channel}}", value: "{{.args.message}}")
}

type Subscription {
  alerts(channel: String!): JSON @redis(operation: SUBSCRIBE, channel: "alerts:{{.args.channel}}")
}
```
