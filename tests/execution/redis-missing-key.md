---
error: true
---

# redis-missing-key

```yaml @config
links:
  - id: "default"
    type: Redis
    src: "redis://localhost:6379"
```

```graphql @schema
schema {
  query: Query
}

type Query {
  cachedValue: String @redis
}
```
