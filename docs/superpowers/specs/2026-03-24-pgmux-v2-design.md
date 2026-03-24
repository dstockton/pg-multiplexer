# PgMux v2 Design Spec

Rename from pg-multiplexer to PgMux. Replace hand-rolled protocol auth with `postgres-protocol`. Migrate metrics to `prometheus-client`. Multi-arch distroless Docker image published to GHCR. Update all dependencies. Default upstream TLS on. Add docker-compose for local development. Change license to Apache 2.0. New README for product positioning.

## 1. Rename: pg-multiplexer → PgMux

### What changes

| Item | Before | After |
|---|---|---|
| Crate name | `pg-multiplexer` | `pgmux` |
| Binary name | `pg-multiplexer` | `pgmux` |
| Docker image | `pg-multiplexer:test` | `ghcr.io/dstockton/pgmux` |
| Config path (Docker) | `/etc/pg-multiplexer/config.toml` | `/etc/pgmux/config.toml` |
| Env var prefix | `PG_MUX_` | `PG_MUX_` (unchanged — already short) |
| Metrics prefix | `pgmux_` | `pgmux_` (unchanged) |
| Rust module | `pg_multiplexer` | `pgmux` |
| CI workflow references | `pg-multiplexer` | `pgmux` |
| README title | pg-multiplexer | PgMux |

### Files affected

- `Cargo.toml` (name, binary name)
- `Dockerfile` (binary path, config path)
- `config.toml` (comments only)
- `.github/workflows/ci.yml`
- `src/main.rs` (command name/about)
- `src/lib.rs` (no code changes needed — module names unchanged, but crate name changes affect external imports)
- `tests/integration_test.rs` (crate import: `pg_multiplexer` → `pgmux`)
- `README.md` (full rewrite)
- `docker-compose.yml` (new file, uses new name)

## 2. Backend Auth — postgres-protocol

### Problem

`backend.rs` (442 lines) contains ~220 lines of hand-rolled auth code: SCRAM-SHA-256 (~185 lines), MD5 password hashing, and PBKDF2-HMAC-SHA256. This code must be updated whenever Postgres adds or changes auth mechanisms.

### Solution

Replace the auth handling in `connect_backend()` with the `postgres-protocol` crate. This is the low-level crate that `tokio-postgres` uses internally. It exposes:

- `postgres_protocol::authentication::sasl::ScramSha256` — full SCRAM client
- `postgres_protocol::authentication::md5_hash` — MD5 password hashing
- `postgres_protocol::message::frontend` — message builders (startup, password)
- `postgres_protocol::message::backend` — message parsers (auth responses, errors)

### What stays custom

- TCP connection setup (we need the raw `TcpStream` for the proxy loop)
- Startup message construction (forwards `extra_params` to backend; `max_db_size` is stripped by `frontend.rs` before forwarding)
- `reset_connection()` and `health_check()` — send raw queries on the stream
- All of `frontend.rs` (proxy loop, read-only injection, query interception)
- All of `messages.rs` (message framing, query extraction, error/notice builders)

### What gets removed

- Hand-rolled SCRAM-SHA-256 implementation (~185 lines including PBKDF2)
- Hand-rolled MD5 password computation
- Crates: `md5`, `sha2`, `hmac`, `pbkdf2`
- `byteorder` (redundant with `bytes`)

### What gets added

- `postgres-protocol` crate (pulls in its own crypto deps internally)

### Architecture after change

```
Client → [frontend.rs: startup parse, cleartext password collection]
       → [pool/mod.rs: acquire connection]
       → [backend.rs: TCP connect → startup msg → postgres-protocol auth → raw TcpStream]
       → [frontend.rs: proxy loop with message interception]
```

The proxy loop continues to use custom `messages.rs` framing because no library exposes per-message interception of an active PG session.

## 3. prometheus-client Migration

### Before (prometheus 0.13)

```rust
use prometheus::{IntCounter, IntGauge, GaugeVec, Opts, Registry};
// Manual Box::new + register for each metric
```

### After (prometheus-client 0.24)

```rust
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;
use prometheus_client::encoding::EncodeLabelSet;
```

### Key differences

- `IntCounter` → `Counter<u64>`
- `IntGauge` → `Gauge<i64>`
- `GaugeVec` with string labels → `Family<DatabaseLabels, Gauge<f64>>` with a typed label struct
- Registration: `registry.register("name", "help", metric.clone())` — no `Box::new`
- Encoding: `prometheus_client::encoding::text::encode(&mut String, &registry)`
- Label structs derive `EncodeLabelSet`:

```rust
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct DatabaseLabels {
    database: String,
}
```

### Files affected

- `src/admin/metrics.rs` — full rewrite of struct + constructor
- `src/admin/server.rs` — `metrics_handler` uses new encoder
- `src/monitor/mod.rs` — metric update calls change syntax
- `src/pool/mod.rs` — metric update calls change syntax
- `src/main.rs` — metric update calls change syntax

### Metric names preserved

All `pgmux_*` metric names stay identical for Grafana/dashboard compatibility.

## 4. Dockerfile — Distroless Multi-arch

### Build stage

```dockerfile
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
ARG TARGETPLATFORM
ARG TARGETARCH
```

Cross-compilation setup for arm64 when building on amd64 (install `gcc-aarch64-linux-gnu`, set linker in cargo config). Map `TARGETARCH` to Rust target triple.

### Runtime stage

```dockerfile
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/<target>/release/pgmux /usr/local/bin/pgmux
COPY config.toml /etc/pgmux/config.toml
EXPOSE 5433 9090
ENTRYPOINT ["pgmux"]
CMD ["--config", "/etc/pgmux/config.toml"]
```

Removed vs current Dockerfile:
- No `apt-get install ca-certificates` (included in distroless)
- No `useradd` (distroless `nonroot` tag runs as UID 65532)
- No `HEALTHCHECK` (no shell — health checks done by orchestrator or docker-compose)
- TLS cert path comments in `config.toml` updated from `/etc/pg-multiplexer/` to `/etc/pgmux/`

### Why `cc-debian12` not `static-debian12`

The binary links dynamically against glibc (standard Rust on Linux). `cc-debian12` includes glibc + libgcc + ca-certificates. `static-debian12` has none of these — it requires musl static linking, which has known performance issues (DNS resolution, memory allocation) unsuitable for a connection multiplexer.

## 5. CI — Build and Push to GHCR

### Replaces the current `docker` job

```yaml
docker:
  name: Docker Build & Push
  runs-on: ubuntu-latest
  needs: [check, unit-tests]
  permissions:
    contents: read
    packages: write
  steps:
    - uses: actions/checkout@v4
    - uses: docker/setup-qemu-action@v3
    - uses: docker/setup-buildx-action@v3
    - uses: docker/login-action@v3
      with:
        registry: ghcr.io
        username: ${{ github.actor }}
        password: ${{ secrets.GITHUB_TOKEN }}
    - uses: docker/metadata-action@v5
      id: meta
      with:
        images: ghcr.io/dstockton/pgmux
        tags: |
          type=sha
          type=ref,event=branch
          type=semver,pattern={{version}}
          type=raw,value=latest,enable={{is_default_branch}}
    - uses: docker/build-push-action@v6
      with:
        context: .
        platforms: linux/amd64,linux/arm64
        push: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}
        tags: ${{ steps.meta.outputs.tags }}
        labels: ${{ steps.meta.outputs.labels }}
        cache-from: type=gha
        cache-to: type=gha,mode=max
```

Push only on main. PRs build both platforms but do not push.

## 6. docker-compose.yml

```yaml
services:
  postgres:
    image: postgres:17
    ports:
      - "15432:5432"
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: postgres
      POSTGRES_DB: postgres
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
      timeout: 3s
      retries: 5

  pgmux:
    build: .
    ports:
      - "15433:5433"
      - "19090:9090"
    environment:
      PG_MUX_UPSTREAM_HOST: postgres
      PG_MUX_UPSTREAM_PORT: "5432"
      PG_MUX_UPSTREAM_TLS: "false"
    depends_on:
      postgres:
        condition: service_healthy
```

Ports chosen to avoid conflicts with local Postgres (5432) or other services:
- 15432 — Postgres direct access
- 15433 — PgMux PG protocol
- 19090 — PgMux admin dashboard

## 7. Config Defaults

Change `upstream_tls` default from `false` to `true` in both:
- `config.toml` (the shipped default config file)
- `TlsConfig::default()` (the Rust default impl)

Update `config.toml` comment to note this is now on by default.

For docker-compose local dev, the compose file sets `PG_MUX_UPSTREAM_TLS=false` since the local Postgres container doesn't have TLS configured.

Need to add `PG_MUX_UPSTREAM_TLS` as a recognized env var override in `Config::load()` — it currently isn't handled there.

### Breaking change

Changing `upstream_tls` default from `false` to `true` breaks existing deployments where the upstream Postgres does not have TLS configured. Connections will fail with a TLS handshake error. Users must set `upstream_tls = false` in config or `PG_MUX_UPSTREAM_TLS=false` in environment to restore previous behavior.

## 8. License

- `Cargo.toml`: `license = "Apache-2.0"`
- Add `LICENSE` file with full Apache 2.0 text
- README footer: "Apache 2.0"

## 9. README

Full rewrite using the user-provided README (see below), with one correction: remove "optional Redis" from the stateless architecture bullet (Redis integration does not exist).

Add a "Roadmap" section that calls out features not yet implemented.

### README content

```markdown
# PgMux

Multi-tenant connection multiplexing for Postgres.

PgMux sits between your application and Postgres, allowing you to safely
run large numbers of isolated tenants on a single database cluster.

---

## Why PgMux?

Modern SaaS applications often need to support hundreds or thousands of
tenants, each with their own database or credentials.

Postgres itself doesn't provide strong controls for:
- limiting database size per tenant
- isolating noisy neighbours
- managing connection pressure across many tenants

PgMux solves this by acting as a lightweight, tenant-aware gateway.

---

## Key Features

- Connection multiplexing across many databases and users
- Tenant-aware routing and isolation
- Per-tenant database size limits with automatic write restriction
- Connection pool limits per tenant and globally
- Admin dashboard with real-time metrics
- Prometheus-compatible metrics endpoint
- Designed for serverless and multi-tenant environments

---

## Use Cases

- SaaS platforms running one database per tenant
- Serverless Postgres providers
- Platforms with untrusted or semi-trusted tenants
- High-density multi-tenant systems

---

## How It Works

PgMux accepts Postgres client connections and routes them to upstream
Postgres based on the database and user provided at connect time.

It can:
- enforce per-tenant database size limits (automatic read-only when exceeded)
- allow shrink operations (DELETE, TRUNCATE, DROP) even when over limit
- pool and reuse backend connections across tenant sessions
- expose pool stats, database sizes, and health via HTTP API and dashboard

---

## Roadmap

The following are natural next steps, not yet implemented:

- **Rate limiting / QPS throttling** per tenant
- **Query-level isolation** (resource quotas beyond connection and size limits)
- **Redis-backed shared state** for running multiple PgMux instances
- **Full client-side TLS termination** (currently responds to SSL requests
  but does not complete the TLS handshake)
- **Extended query protocol interception** for read-only enforcement
  (currently only simple query protocol is intercepted)
- **Configurable admin credentials** for the DB size monitor
  (currently hardcoded to postgres/postgres)
- **Graceful shutdown** with connection draining
- **Hot config reload** without restart

---

## Getting Started

### Docker Compose (quickest)

    docker compose up

This starts Postgres on port 15432 and PgMux on port 15433
(admin dashboard on port 19090).

Connect through PgMux:

    psql -h localhost -p 15433 -U postgres -d postgres

View the dashboard at http://localhost:19090

### Docker

    docker pull ghcr.io/dstockton/pgmux:latest
    docker run -p 5433:5433 -p 9090:9090 \
      -e PG_MUX_UPSTREAM_HOST=host.docker.internal \
      pgmux:latest

### From Source

    cargo build --release
    ./target/release/pgmux --config config.toml

---

## Configuration

See config.toml for all options with defaults and documentation.

Key environment variable overrides:
- PG_MUX_UPSTREAM_HOST — upstream Postgres host
- PG_MUX_UPSTREAM_PORT — upstream Postgres port
- PG_MUX_LISTEN — PgMux listen address
- PG_MUX_ADMIN_LISTEN — admin HTTP listen address
- PG_MUX_TLS_CERT / PG_MUX_TLS_KEY — enable client-facing TLS
- PG_MUX_UPSTREAM_TLS — use TLS to upstream (default: true)

---

## Status

Early stage — feedback and contributions welcome.

---

## License

Apache 2.0

---

## Author

Created and maintained by David Stockton.

If you're using PgMux in production, a star on GitHub is appreciated.
```

## 10. Not Yet Implemented (Roadmap)

These features are referenced or implied by the README but do not exist in the codebase. They are listed in the README's Roadmap section for transparency.

| Feature | Current state | Gap |
|---|---|---|
| Rate limiting / QPS throttling | Not implemented | No per-tenant query rate control |
| Query-level resource isolation | Connection-level only | No CPU/memory/IO quotas per tenant |
| Redis shared state | All state is in-memory | Cannot run multiple PgMux instances with shared pool state |
| Client-side TLS handshake | Responds `S` to SSL request but warns and continues plaintext | TLS acceptor is built but stream wrapping is not wired up |
| Extended query protocol read-only | Only simple query (`Q` message) intercepted | `Parse` messages (`P`) are forwarded as-is with a TODO comment |
| Admin credentials for size monitor | Hardcoded `postgres`/`postgres` | Should be configurable in config.toml |
| Graceful shutdown | None — process exits immediately | No connection draining or SIGTERM handling |
| Hot config reload | Requires restart | No SIGHUP or file-watch support |

## 11. Dependency Summary (final state)

### Dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
bytes = "1"
postgres-protocol = "0.6"
tokio-rustls = "0.26"
rustls = "0.23"
rustls-pemfile = "2"
axum = { version = "0.8", features = ["ws"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "fs"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"
prometheus-client = "0.24"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
rand = "0.10"
dashmap = "6"
parking_lot = "0.12"
url = "2"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "2"
arc-swap = "1"
tokio-postgres = { version = "0.7", features = ["with-serde_json-1"] }

[dev-dependencies]
tokio-test = "0.4"
tempfile = "3"
criterion = "0.8"
assert_cmd = "2"
predicates = "3"
reqwest = { version = "0.13", features = ["json"] }
tokio-postgres = { version = "0.7" }
```

### Removed

- `md5` — auth handled by postgres-protocol
- `sha2` — auth handled by postgres-protocol
- `hmac` — auth handled by postgres-protocol
- `pbkdf2` — auth handled by postgres-protocol
- `base64` — only used for SCRAM auth, now handled by postgres-protocol
- `byteorder` — redundant with `bytes`
- `prometheus` — replaced by `prometheus-client`

### Added

- `postgres-protocol` — PG wire protocol auth (SCRAM, MD5)
- `prometheus-client` — OpenMetrics-compliant metrics

### Notable version bumps

| Crate | Before | After | Notes |
|---|---|---|---|
| `toml` | 0.8 | 1 | Major version; API compatible for deserialization |
| `rand` | 0.9 | 0.10 | Minor API changes in random generation |
| `criterion` | 0.5 | 0.8 | Dev-only; benchmark harness |
| `reqwest` | 0.12 | 0.13 | Dev-only; test HTTP client |
