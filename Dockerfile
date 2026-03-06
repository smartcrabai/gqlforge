FROM rust:1.94-bookworm AS builder
RUN apt-get update && apt-get install -y cmake perl pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --bin gqlforge

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/gqlforge /usr/local/bin/gqlforge
ENTRYPOINT ["gqlforge"]
