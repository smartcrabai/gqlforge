+++
title = "@redis Directive"
description = "Resolve GraphQL fields by executing commands against Redis."
+++

# @redis Directive

The `@redis` directive resolves a field by executing a command against a Redis (or Redis-compatible) server. It requires a connection linked via `@link(type: Redis, src: "redis://...")`. Use the `rediss://` scheme instead of `redis://` to connect over TLS.

## Fields

| Field         | Type             | Default | Description                                                                                                    |
| ------------- | ---------------- | ------- | -------------------------------------------------------------------------------------------------------------- |
| `db`          | String           | `null`  | The `@link(type: Redis)` id to use. Optional when only one Redis link is defined.                              |
| `operation`   | RedisOperation   | `GET`   | The Redis command to run. See below.                                                                           |
| `key`         | String           | `null`  | The key to operate on. Supports Mustache templates.                                                            |
| `field`       | String           | `null`  | The hash field to operate on (`HGET`/`HSET`). Supports Mustache templates.                                     |
| `value`       | String           | `null`  | The value to write (`SET`/`HSET`/`LPUSH`/`RPUSH`/`SADD`/`PUBLISH`/`XADD`). Supports Mustache templates.        |
| `ttl`         | String           | `null`  | Expiry in seconds for `SET` (`EX <ttl>`). Supports Mustache templates. Omitted (empty render) means no expiry. |
| `start`       | String           | `"0"`   | Start index for `LRANGE`. Supports Mustache templates.                                                         |
| `stop`        | String           | `"-1"`  | Stop index for `LRANGE`. Supports Mustache templates.                                                          |
| `channel`     | String           | `null`  | The Pub/Sub channel to publish to or subscribe on.                                                             |
| `startId`     | String           | `"$"`   | The Redis Stream id to start reading from (`XREAD`). Supports Mustache templates.                              |
| `payloadType` | RedisPayloadType | `JSON`  | How to interpret string payloads. See below.                                                                   |
| `dedupe`      | Boolean          | `false` | Deduplicate identical in-flight Redis calls. Not applicable to `SUBSCRIBE`/`XREAD`.                            |

## RedisOperation

| Value       | Required Fields         | Notes                                                                                                                                                             |
| ----------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET`       | `key`                   | Default operation.                                                                                                                                                |
| `SET`       | `key`, `value`          | `ttl` is optional and adds an `EX <ttl>` expiry. **Returns `Boolean`**: `true` on success, `false` if the write was rejected (e.g. a future `NX`/`XX` condition). |
| `DEL`       | `key`                   |                                                                                                                                                                   |
| `EXISTS`    | `key`                   | **Returns `Boolean`**: `true` if the key exists, `false` otherwise (Redis' integer count is normalized to a boolean).                                             |
| `INCR`      | `key`                   |                                                                                                                                                                   |
| `HGET`      | `key`, `field`          |                                                                                                                                                                   |
| `HSET`      | `key`, `field`, `value` |                                                                                                                                                                   |
| `HGETALL`   | `key`                   | Always resolves as an object/map, regardless of whether the server negotiated RESP2 or RESP3.                                                                     |
| `LPUSH`     | `key`, `value`          |                                                                                                                                                                   |
| `RPUSH`     | `key`, `value`          |                                                                                                                                                                   |
| `LRANGE`    | `key`                   | `start`/`stop` are optional and default to `"0"`/`"-1"` (the entire list).                                                                                        |
| `SADD`      | `key`, `value`          |                                                                                                                                                                   |
| `SMEMBERS`  | `key`                   |                                                                                                                                                                   |
| `PUBLISH`   | `channel`, `value`      |                                                                                                                                                                   |
| `XADD`      | `key`, `value`          | See [XADD payload expansion](#xadd-payload-expansion) below.                                                                                                      |
| `SUBSCRIBE` | `channel`               | **Subscription fields only.**                                                                                                                                     |
| `XREAD`     | `key`                   | **Subscription fields only.** `startId` is optional and defaults to `"$"` (new entries).                                                                          |

`SUBSCRIBE` and `XREAD` may only be used on fields under the `Subscription` root type. Conversely, every other operation is rejected on `Subscription` fields — a `Subscription` field must set `operation: SUBSCRIBE` or `operation: XREAD` explicitly.

`SET` and `EXISTS` return Redis' native reply (`+OK`/null bulk, and an integer count, respectively) normalized to a GraphQL `Boolean` — declare the field as `Boolean` (or `Boolean!`) to match, as in the examples above. `HGETALL` returns an object either way, whether the server replied with RESP2's flat `[field, value, ...]` array or RESP3's native map.

### XADD payload expansion

`XADD` appends an entry to a Redis Stream. The rendered `value` is parsed as JSON:

- If it is a JSON **object**, each key/value pair becomes a separate field in the stream entry (e.g. `{"name": "Alice", "age": 30}` becomes the fields `name`/`Alice` and `age`/`30`).
- Otherwise (a plain string, number, or invalid JSON), the raw rendered value is stored under a single field named `payload`.

Because of this, prefer sending a JSON object as `value` when you want individually queryable fields on the stream entry.

## RedisPayloadType

| Value  | Description                                                                                 |
| ------ | ------------------------------------------------------------------------------------------- |
| `JSON` | Default. Tries to parse the payload as JSON; falls back to a plain string if parsing fails. |
| `TEXT` | Always treats the payload as a raw string, never attempting JSON parsing.                   |

`payloadType` is applied recursively to every string leaf of the resolved value — for example, both the top-level `GET` result and each value inside an `XREAD`/`HGETALL`-style map are decoded independently.

Because `JSON` decoding runs on every string leaf, a stored string that merely _looks_ like a number or boolean (e.g. `"1"`, `"true"`) is decoded into that JSON type rather than kept as a literal string. Use `payloadType: TEXT` on fields where you need the raw string back verbatim. This decoding is independent of the `Boolean` normalization `SET`/`EXISTS` get above, which happens regardless of `payloadType`.

## Examples

### Simple GET

```graphql
type Query {
  cachedValue(key: String!): String @redis(key: "cache:{{.args.key}}")
}
```

`operation` defaults to `GET`, so it can be omitted.

### SET with an optional TTL

```graphql
type Mutation {
  cacheValue(key: String!, value: String!, ttlSeconds: Int): Boolean
  @redis(
    operation: SET
    key: "cache:{{.args.key}}"
    value: "{{.args.value}}"
    ttl: "{{.args.ttlSeconds}}"
  )
}
```

When `ttlSeconds` is not supplied, the rendered `ttl` template is empty and no expiry is set.

### Hash operations

```graphql
type Query {
  userField(id: ID!, field: String!): String @redis(operation: HGET, key: "user:{{.args.id}}", field: "{{.args.field}}")

  userProfile(id: ID!): JSON @redis(operation: HGETALL, key: "user:{{.args.id}}")
}

type Mutation {
  setUserField(id: ID!, field: String!, value: String!): Boolean
  @redis(
    operation: HSET
    key: "user:{{.args.id}}"
    field: "{{.args.field}}"
    value: "{{.args.value}}"
  )
}
```

### List and set operations

```graphql
type Query {
  recentEvents(stop: Int): [String!]! @redis(operation: LRANGE, key: "events:recent", stop: "{{.args.stop}}")

  tags: [String!]! @redis(operation: SMEMBERS, key: "tags:all")
}

type Mutation {
  pushEvent(event: String!): Int @redis(operation: RPUSH, key: "events:recent", value: "{{.args.event}}")

  addTag(tag: String!): Int @redis(operation: SADD, key: "tags:all", value: "{{.args.tag}}")
}
```

### Publishing to a channel

```graphql
type Mutation {
  notify(channel: String!, message: String!): Int
  @redis(operation: PUBLISH, channel: "alerts:{{.args.channel}}", value: "{{.args.message}}")
}
```

### Appending to a stream

```graphql
type Mutation {
  recordEvent(streamKey: String!, payload: JSON!): String
  @redis(operation: XADD, key: "stream:{{.args.streamKey}}", value: "{{.args.payload}}")
}
```

### Multiple Redis connections

When more than one `@link(type: Redis)` is defined, use `db` to select which connection a field talks to:

```graphql
schema
@server(port: 8000)
@link(id: "cache", type: Redis, src: "redis://localhost:6379")
@link(id: "pubsub", type: Redis, src: "rediss://pubsub.internal:6380") {
  query: Query
  mutation: Mutation
  subscription: Subscription
}

type Query {
  cachedValue(key: String!): String @redis(db: "cache", key: "cache:{{.args.key}}")
}

type Mutation {
  notify(channel: String!, message: String!): Int
  @redis(db: "pubsub", operation: PUBLISH, channel: "alerts:{{.args.channel}}", value: "{{.args.message}}")
}
```

When only one `@link(type: Redis)` is defined, `db` can be omitted. If two or more Redis links exist, `db` is required — otherwise the schema fails to compile.

## Subscriptions

`@redis` on a `Subscription` field must use `operation: SUBSCRIBE` (Pub/Sub) or `operation: XREAD` (Streams). Both deliver events to GraphQL subscribers over the server's SSE (Server-Sent Events) transport: sending a subscription operation via `POST` returns a `text/event-stream` response, and each Redis message/entry is delivered as one SSE event.

### SUBSCRIBE (Pub/Sub)

```graphql
type Subscription {
  notifications(channel: String!): JSON @redis(operation: SUBSCRIBE, channel: "alerts:{{.args.channel}}")
}
```

```bash
curl -N -X POST http://localhost:8000/graphql \
  -H "Content-Type: application/json" \
  -d '{"query": "subscription { notifications(channel: \"alerts\") }"}'
```

### XREAD (Streams)

```graphql
type Subscription {
  streamEvents(key: String!): JSON @redis(operation: XREAD, key: "stream:{{.args.key}}")
}
```

```bash
curl -N -X POST http://localhost:8000/graphql \
  -H "Content-Type: application/json" \
  -d '{"query": "subscription { streamEvents(key: \"events\") }"}'
```

Each delivered event has the shape:

```json
{ "id": "1699999999999-0", "values": { "name": "Alice", "age": "30" } }
```

`id` is the Stream entry id and `values` is a map of the entry's fields, with each value decoded according to `payloadType`.

### Delivery semantics

| Operation   | Persistence                            | Semantics                                                                                                                                                          |
| ----------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `SUBSCRIBE` | Pub/Sub messages are not persisted.    | **At-most-once.** Messages published while the client is disconnected (e.g. during a reconnect) are lost.                                                          |
| `XREAD`     | Stream entries are persisted by Redis. | **At-least-once–leaning.** On automatic reconnect, GQLForge resumes reading from the last entry id it successfully delivered, rather than restarting at `startId`. |

Choose `SUBSCRIBE` for ephemeral, fire-and-forget notifications, and `XREAD` when you need delivery to survive brief client or network interruptions.

## Security

Mustache-rendered values are passed to Redis as discrete, positional command arguments — they are never concatenated into a single command string — so there is no Redis equivalent of SQL injection. However, `key`, `field`, and `channel` templates built from user-supplied arguments can still let a client read or write arbitrary keys/channels. Scope such templates to a fixed namespace (e.g. `"user:{{.args.id}}"` rather than `"{{.args.rawKey}}"`) when the argument is untrusted.
