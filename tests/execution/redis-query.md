# redis-query

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
  mutation: Mutation
}

type Query {
  cachedValue(key: String!): String @redis(key: "{{.args.key}}")
  userProfile(id: ID!): JSON @redis(operation: HGETALL, key: "user:{{.args.id}}")
}

type Mutation {
  cacheValue(key: String!, value: String!): Boolean
  @redis(operation: SET, key: "{{.args.key}}", value: "{{.args.value}}")
}
```
