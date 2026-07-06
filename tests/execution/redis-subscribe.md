# redis-subscribe

```yaml @config
server:
  port: 8000
links:
  - id: "default"
    type: Redis
    src: "redis://localhost:6379"
```

```graphql @schema
schema {
  query: Query
  subscription: Subscription
}

type Query {
  dummy: String @expr(body: "ok")
}

type Subscription {
  alerts(channel: String!): JSON @redis(operation: SUBSCRIBE, channel: "{{.args.channel}}")
  streamEvents(key: String!): JSON @redis(operation: XREAD, key: "{{.args.key}}")
}
```
