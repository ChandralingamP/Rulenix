FROM rust:1.88-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /nonexistent --shell /usr/sbin/nologin rulenix
WORKDIR /app
COPY --from=builder /app/target/release/rulenix-backend /usr/local/bin/rulenix-backend
USER rulenix
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/rulenix-backend"]
