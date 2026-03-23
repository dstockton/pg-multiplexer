# =============================================================================
# pg-multiplexer Docker build
# Multi-stage build for minimal final image
# =============================================================================

# Stage 1: Build
FROM rust:1.83-bookworm AS builder

RUN apt-get update && apt-get install -y \
    cmake \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency build
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

# Build the actual application
COPY src/ src/
COPY tests/ tests/
RUN touch src/main.rs && cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /usr/sbin/nologin pgmux

COPY --from=builder /app/target/release/pg-multiplexer /usr/local/bin/pg-multiplexer

# Default config
COPY config.toml /etc/pg-multiplexer/config.toml

USER pgmux

EXPOSE 5433 9090

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:9090/health || exit 1

ENTRYPOINT ["pg-multiplexer"]
CMD ["--config", "/etc/pg-multiplexer/config.toml"]
