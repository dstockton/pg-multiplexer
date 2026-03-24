# pg-multiplexer

A high-performance Postgres connection multiplexer written in Rust, designed for multi-tenant SaaS deployments where each tenant connects to a unique database with different credentials on the same Postgres server.

## The Problem

Multi-tenant SaaS platforms that deploy one process per tenant, each connecting to its own database, face a scaling challenge: thousands of client connections map 1:1 to backend Postgres connections. PgBouncer can't help because each connection uses different credentials and databases.

## The Solution

pg-multiplexer sits between your application processes and Postgres, managing connection pools keyed by `(host, port, database, user)`. It:

- Handles **10,000s of client connections** with only **100s-1000s of backend connections**
- Pools connections per unique `(db, user)` combination
- Supports **cleartext, MD5, and SCRAM-SHA-256** authentication
- Enforces **database size limits** by switching oversized databases to read-only mode
- Is **fully stateless** — run multiple instances with no shared state
- Provides a **real-time admin dashboard** with charts and Prometheus metrics

## Architecture

```
Clients (10,000s)
    │  Postgres wire protocol
    ▼
┌─────────────────────────────────┐
│  pg-multiplexer                 │
│                                 │
│  ┌──────────┐  ┌──────────────┐│
│  │ Protocol  │  │ Pool Manager ││
│  │ Frontend  │  │ (per db/user)││
│  └─────┬─────┘  └──────┬──────┘│
│        │               │       │
│  ┌─────┴───────────────┴─────┐ │
│  │  DB Size Monitor          │ │
│  │  (periodic pg_db_size())  │ │
│  └───────────────────────────┘ │
│                                 │
│  ┌───────────────────────────┐ │
│  │ HTTP: /admin /metrics     │ │
│  └───────────────────────────┘ │
└─────────────────────────────────┘
    │  Postgres wire protocol
    ▼
Postgres (100s of databases)
```

## Quick Start

### Binary

```bash
# Start with default config (listens on :5433, admin on :9090)
pg-multiplexer --config config.toml

# Override via CLI
pg-multiplexer --listen 0.0.0.0:6432 --admin-listen 0.0.0.0:8080

# Override via environment
PG_MUX_UPSTREAM_HOST=pg.example.com PG_MUX_UPSTREAM_PORT=5432 pg-multiplexer
```

### Docker

```bash
docker run -p 5433:5433 -p 9090:9090 \
  -e PG_MUX_UPSTREAM_HOST=pg.example.com \
  pg-multiplexer
```

### Connect Through the Multiplexer

Connect your application to `pg-multiplexer` instead of directly to Postgres:

```bash
# Before: direct connection
psql "host=pg.example.com port=5432 dbname=tenant_42 user=tenant_42 password=secret"

# After: through multiplexer
psql "host=localhost port=5433 dbname=tenant_42 user=tenant_42 password=secret"
```

## Database Size Limits

Enforce per-database size limits to prevent any single tenant from consuming too much storage. When a database exceeds its limit, the multiplexer switches it to **read-only mode**.

### Setting Limits

Pass `max_db_size` as a startup parameter in the connection string:

```bash
# Via connection options
psql "host=localhost port=5433 dbname=tenant_42 user=tenant_42 password=secret options='--max_db_size=5GB'"
```

Or set a global default in `config.toml`:

```toml
[monitor]
default_max_db_size_bytes = 5368709120  # 5GB
check_interval_secs = 60
allow_shrink_operations_when_overlimit = true
```

### Read-Only Enforcement

When a database exceeds its limit:

1. All write queries receive `SET TRANSACTION READ ONLY` injection
2. **DELETE, TRUNCATE, DROP, and VACUUM are still allowed** so tenants can reduce their size
3. Clients receive a PostgreSQL `NOTICE` explaining the restriction
4. The dashboard shows the database as `READ-ONLY` with a red indicator

## Configuration

See [`config.toml`](config.toml) for all options with documentation. Configuration is loaded from:

1. Config file (default: `config.toml`)
2. CLI flags (`--listen`, `--admin-listen`)
3. Environment variables (`PG_MUX_LISTEN`, `PG_MUX_ADMIN_LISTEN`, `PG_MUX_UPSTREAM_HOST`, `PG_MUX_UPSTREAM_PORT`, `PG_MUX_TLS_CERT`, `PG_MUX_TLS_KEY`)

Later sources override earlier ones.

### Pool Configuration

| Setting | Default | Description |
|---------|---------|-------------|
| `pool.max_connections_per_pool` | 20 | Max backend connections per (db, user) |
| `pool.max_total_connections` | 500 | Global backend connection limit |
| `pool.acquire_timeout_ms` | 5000 | Wait timeout for backend connection |
| `pool.idle_timeout_secs` | 300 | Evict idle connections after this |
| `pool.max_connection_lifetime_secs` | 3600 | Force-recycle connections after this |

### TLS Configuration

```toml
[tls]
enabled = true
cert_path = "/etc/pg-multiplexer/server.crt"
key_path = "/etc/pg-multiplexer/server.key"
require_tls = false
upstream_tls = false
```

## Admin Dashboard

Access the dashboard at `http://localhost:9090/`:

- **Real-time charts** for connection counts and database sizes
- **Connection pool stats** per database/user combination
- **Database size enforcement** status with progress bars
- **Auto-refresh** every 5 seconds

### API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /` | Admin dashboard (HTML) |
| `GET /health` | Health check (JSON) |
| `GET /metrics` | Prometheus metrics |
| `GET /api/stats` | Combined stats (JSON) |
| `GET /api/pools` | Pool stats (JSON) |
| `GET /api/databases` | Database size info (JSON) |

### Prometheus Metrics

Key metrics exposed at `/metrics`:

```
pgmux_client_connections_active    # Current client connections
pgmux_server_connections_active    # Current backend connections
pgmux_pool_hits_total              # Connections served from pool
pgmux_pool_misses_total            # New backend connections created
pgmux_pool_timeouts_total          # Connection acquire timeouts
pgmux_db_size_bytes{database}      # Current database size
pgmux_db_size_limit_bytes{database}# Configured size limit
pgmux_db_over_limit{database}      # 1 if over limit, 0 if ok
```

## Building

### From Source

```bash
cargo build --release
# Binary at target/release/pg-multiplexer
```

### Docker Image

```bash
docker build -t pg-multiplexer .
```

### Cross-Compilation

CI produces binaries for:
- `linux-amd64`
- `linux-arm64`
- `macos-arm64`

## Testing

```bash
# Unit tests (no Postgres needed)
cargo test --lib

# Integration tests (requires Postgres on localhost:5432)
PG_TEST_HOST=localhost PG_TEST_PORT=5432 \
PG_TEST_USER=postgres PG_TEST_PASSWORD=postgres \
cargo test --test integration_test -- --nocapture
```

## Design Decisions

### Stateless Architecture

Each multiplexer instance operates independently:
- Connection pools are local to each instance
- DB size checks run independently per instance (slightly redundant, but no coordination needed)
- No Redis or shared state required
- Scale horizontally behind a TCP load balancer

### Session-Level Pooling

Connections are held for the duration of a client session. This maximizes compatibility with applications like Directus that may use session-level features (SET commands, prepared statements, temp tables). The multiplexer still provides value because:
- Idle client sessions don't hold backend connections (connections are evicted after idle timeout)
- Backend connections are reused across different client sessions
- Connection establishment overhead is amortized

### Wire Protocol Proxying

The multiplexer implements the Postgres wire protocol v3 directly rather than using a Postgres client library for proxying. This gives:
- Full control over message inspection and injection
- Ability to intercept and modify queries (for read-only enforcement)
- Minimal overhead (no SQL parsing, just binary protocol relay)
- Support for both simple and extended query protocols

## License

MIT
