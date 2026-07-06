---
error: true
---

# redis-subscribe-on-query

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
  alerts: JSON @redis(operation: SUBSCRIBE, channel: "alerts")
}
```
