# syntax=docker/dockerfile:1

FROM rust:1-alpine AS builder
# aws-lc-rs (rustls crypto backend) needs a C toolchain.
RUN apk add --no-cache musl-dev build-base cmake perl
WORKDIR /app

# Cache dependency builds separately from source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked \
    && strip target/release/ocs-ai-answerer

FROM alpine:3
RUN apk add --no-cache ca-certificates tzdata \
    && adduser -D -H -u 10001 app \
    && mkdir -p /app/data && chown app /app/data
WORKDIR /app
COPY --from=builder /app/target/release/ocs-ai-answerer /usr/local/bin/ocs-ai-answerer
COPY config.example.toml /app/config.example.toml

ENV OCS_SERVER_HOST=0.0.0.0 \
    OCS_SERVER_PORT=3000 \
    OCS_CACHE_FILE=/app/data/cache.jsonl
VOLUME ["/app/data"]
EXPOSE 3000
USER app

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -qO- http://127.0.0.1:3000/health >/dev/null || exit 1

ENTRYPOINT ["ocs-ai-answerer"]
