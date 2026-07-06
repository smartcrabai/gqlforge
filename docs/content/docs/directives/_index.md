+++
title = "Directives Overview"
description = "Overview of all available GQLForge directives."
+++

# Directives Overview

GQLForge uses GraphQL schema directives to declaratively configure data fetching, transformation, and access control. Below is a summary of every available directive.

## Data Source Directives

| Directive                                  | Description                                                     |
| ------------------------------------------ | --------------------------------------------------------------- |
| [@http](@/docs/directives/http.md)         | Resolve a field by calling a REST/HTTP endpoint.                |
| [@grpc](@/docs/directives/grpc.md)         | Resolve a field by calling a gRPC service method.               |
| [@graphQL](@/docs/directives/graphQL.md)   | Resolve a field by proxying to another GraphQL server.          |
| [@call](@/docs/directives/call.md)         | Compose multiple resolver steps into a single field resolution. |
| [@expr](@/docs/directives/expr.md)         | Return a static value or a computed expression.                 |
| [@js](@/docs/directives/js.md)             | Resolve a field using a custom JavaScript function.             |
| [@postgres](@/docs/directives/postgres.md) | Resolve a field by querying a PostgreSQL table.                 |
| [@redis](@/docs/directives/redis.md)       | Resolve a field by executing a command against Redis.           |
| [@s3](@/docs/directives/s3.md)             | Resolve a field using Amazon S3 or S3-compatible storage.       |

## Schema Transformation Directives

| Directive                                          | Description                                                |
| -------------------------------------------------- | ---------------------------------------------------------- |
| [@addField](@/docs/directives/addField.md)         | Add a derived field to a type.                             |
| [@modify](@/docs/directives/modify.md)             | Rename a field or mark it as omitted in the public schema. |
| [@omit](@/docs/directives/omit.md)                 | Exclude a field from the public-facing schema entirely.    |
| [@discriminate](@/docs/directives/discriminate.md) | Set the discriminator field for union type resolution.     |

## Performance and Security Directives

| Directive                                    | Description                                              |
| -------------------------------------------- | -------------------------------------------------------- |
| [@cache](@/docs/directives/cache.md)         | Cache a field's resolved value for a specified duration. |
| [@protected](@/docs/directives/protected.md) | Restrict field access to authenticated users.            |

## Endpoint Directives

| Directive                          | Description                                |
| ---------------------------------- | ------------------------------------------ |
| [@rest](@/docs/directives/rest.md) | Expose a GraphQL query as a REST endpoint. |
