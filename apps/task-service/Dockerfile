# ── Build stage ───────────────────────────────────────────────────────────────
FROM rust:1.78-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies separately for faster rebuilds
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release && rm -rf src

# Build actual source
COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs && cargo build --release

# ── Runtime stage ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -ms /bin/bash appuser
USER appuser
WORKDIR /app

COPY --from=builder /app/target/release/task-service ./task-service
COPY --from=builder /app/migrations ./migrations

EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

ENTRYPOINT ["./task-service"]
