# =============================================================================
# pgmux Docker build — multi-arch via buildx
# =============================================================================

# Build stage: use host platform for speed, cross-compile for target
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder

ARG TARGETARCH

# Install cross-compilation tools for arm64
RUN if [ "$TARGETARCH" = "arm64" ]; then \
      apt-get update && apt-get install -y gcc-aarch64-linux-gnu && \
      rm -rf /var/lib/apt/lists/*; \
    fi

WORKDIR /app

# Map Docker arch to Rust target triple
RUN case "$TARGETARCH" in \
      amd64) echo "x86_64-unknown-linux-gnu" > /tmp/rust-target ;; \
      arm64) echo "aarch64-unknown-linux-gnu" > /tmp/rust-target ;; \
      *) echo "unsupported: $TARGETARCH" && exit 1 ;; \
    esac && \
    rustup target add $(cat /tmp/rust-target)

# Configure linker for cross-compilation
RUN if [ "$TARGETARCH" = "arm64" ]; then \
      mkdir -p .cargo && \
      echo '[target.aarch64-unknown-linux-gnu]' > .cargo/config.toml && \
      echo 'linker = "aarch64-linux-gnu-gcc"' >> .cargo/config.toml; \
    fi

# Cache dependency build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    echo 'pub mod admin; pub mod auth; pub mod config; pub mod monitor; pub mod pool; pub mod protocol; pub mod tls;' > src/lib.rs && \
    mkdir -p src/admin src/auth src/protocol src/monitor src/pool src/tls && \
    touch src/admin/mod.rs src/admin/metrics.rs src/admin/server.rs src/auth/mod.rs src/config.rs src/monitor/mod.rs src/pool/mod.rs src/protocol/mod.rs src/protocol/backend.rs src/protocol/frontend.rs src/protocol/messages.rs src/tls/mod.rs
RUN cargo build --release --target $(cat /tmp/rust-target) 2>/dev/null || true
RUN rm -rf src

# Build the actual application
COPY src/ src/
RUN touch src/main.rs && cargo build --release --target $(cat /tmp/rust-target)

# Move binary to predictable location
RUN cp target/$(cat /tmp/rust-target)/release/pgmux /usr/local/bin/pgmux

# Runtime stage: distroless with glibc
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /usr/local/bin/pgmux /usr/local/bin/pgmux
COPY config.toml /etc/pgmux/config.toml

EXPOSE 5433 9090

ENTRYPOINT ["pgmux"]
CMD ["--config", "/etc/pgmux/config.toml"]
