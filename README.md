[![Gqlforge Logo](./assets/logo_light.svg)](https://gqlforge.pages.dev)

Gqlforge is an open-source solution for building high-performance GraphQL backends.

## Installation

### Homebrew (macOS / Linux)

```bash
brew install smartcrabai/tap/gqlforge
```

### Shell (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/smartcrabai/gqlforge/releases/latest/download/gqlforge-installer.sh | sh
```

### Cargo

```bash
cargo install --git https://github.com/smartcrabai/gqlforge
```

### Docker

```bash
docker pull ghcr.io/smartcrabai/gqlforge/gqlforge
docker run -p 8000:8000 -p 8081:8081 ghcr.io/smartcrabai/gqlforge/gqlforge
```

## Get Started

The below file is a standard `.graphQL` file, with a few additions such as `@server` and `@http` directives. So, basically, we specify the GraphQL schema and how to resolve that GraphQL schema in the same file, without having to write any code!

```graphql
schema @server(port: 8000, hostname: "0.0.0.0") @upstream(httpCache: 42) {
  query: Query
}

type Query {
  posts: [Post] @http(url: "http://jsonplaceholder.typicode.com/posts")
  user(id: Int!): User @http(url: "http://jsonplaceholder.typicode.com/users/{{.args.id}}")
}

type User {
  id: Int!
  name: String!
  username: String!
  email: String!
  phone: String
  website: String
}

type Post {
  id: Int!
  userId: Int!
  title: String!
  body: String!
  user: User @http(url: "http://jsonplaceholder.typicode.com/users/{{.value.userId}}")
}
```

Now, run the following command to start the server with the full path to the jsonplaceholder.graphql file that you created above.

```bash
gqlforge start ./jsonplaceholder.graphql
```

Head out to [docs] to learn about other powerful gqlforge features.

[docs]: https://gqlforge.pages.dev/docs

### Support Us

Give us a star.

Watch us for updates.

If you find Gqlforge useful, consider [sponsoring smartcrabai](https://github.com/sponsors/smartcrabai)
to support its development and maintenance.

### Acknowledgements

This project is based on [Tailcall](https://github.com/tailcallhq/tailcall).

### License

This initiative is protected under the Apache 2.0 License.

<img referrerpolicy="no-referrer-when-downgrade" src="https://static.scarf.sh/a.png?x-pxid=82cc2ee2-ff41-4844-9ae6-c9face103e81" />
