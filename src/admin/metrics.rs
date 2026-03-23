use prometheus::{GaugeVec, IntCounter, IntGauge, Opts, Registry};

/// All application metrics.
pub struct Metrics {
    pub registry: Registry,

    // Client metrics
    pub client_connections_total: IntCounter,
    pub client_connections_active: IntGauge,

    // Server/pool metrics
    pub server_connections_total: IntCounter,
    pub server_connections_active: IntGauge,
    pub server_connection_errors_total: IntCounter,
    pub pool_hits_total: IntCounter,
    pub pool_misses_total: IntCounter,
    pub pool_timeouts_total: IntCounter,

    // DB size metrics
    pub db_size_bytes: GaugeVec,
    pub db_size_limit_bytes: GaugeVec,
    pub db_over_limit: GaugeVec,

    // Query metrics
    pub queries_total: IntCounter,
    pub queries_read_only_enforced: IntCounter,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let client_connections_total = IntCounter::new(
            "pgmux_client_connections_total",
            "Total client connections accepted",
        )
        .unwrap();
        let client_connections_active = IntGauge::new(
            "pgmux_client_connections_active",
            "Currently active client connections",
        )
        .unwrap();

        let server_connections_total = IntCounter::new(
            "pgmux_server_connections_total",
            "Total server connections created",
        )
        .unwrap();
        let server_connections_active = IntGauge::new(
            "pgmux_server_connections_active",
            "Currently active server connections",
        )
        .unwrap();
        let server_connection_errors_total = IntCounter::new(
            "pgmux_server_connection_errors_total",
            "Total server connection errors",
        )
        .unwrap();

        let pool_hits_total =
            IntCounter::new("pgmux_pool_hits_total", "Connections served from pool").unwrap();
        let pool_misses_total = IntCounter::new(
            "pgmux_pool_misses_total",
            "Connections that required new backend connection",
        )
        .unwrap();
        let pool_timeouts_total =
            IntCounter::new("pgmux_pool_timeouts_total", "Connection acquire timeouts").unwrap();

        let db_size_bytes = GaugeVec::new(
            Opts::new("pgmux_db_size_bytes", "Current database size in bytes"),
            &["database"],
        )
        .unwrap();
        let db_size_limit_bytes = GaugeVec::new(
            Opts::new(
                "pgmux_db_size_limit_bytes",
                "Configured database size limit in bytes",
            ),
            &["database"],
        )
        .unwrap();
        let db_over_limit = GaugeVec::new(
            Opts::new(
                "pgmux_db_over_limit",
                "Whether database is over its size limit (1=over, 0=ok)",
            ),
            &["database"],
        )
        .unwrap();

        let queries_total =
            IntCounter::new("pgmux_queries_total", "Total queries proxied").unwrap();
        let queries_read_only_enforced = IntCounter::new(
            "pgmux_queries_read_only_enforced",
            "Queries where read-only was enforced due to size limit",
        )
        .unwrap();

        // Register all metrics
        registry
            .register(Box::new(client_connections_total.clone()))
            .unwrap();
        registry
            .register(Box::new(client_connections_active.clone()))
            .unwrap();
        registry
            .register(Box::new(server_connections_total.clone()))
            .unwrap();
        registry
            .register(Box::new(server_connections_active.clone()))
            .unwrap();
        registry
            .register(Box::new(server_connection_errors_total.clone()))
            .unwrap();
        registry
            .register(Box::new(pool_hits_total.clone()))
            .unwrap();
        registry
            .register(Box::new(pool_misses_total.clone()))
            .unwrap();
        registry
            .register(Box::new(pool_timeouts_total.clone()))
            .unwrap();
        registry.register(Box::new(db_size_bytes.clone())).unwrap();
        registry
            .register(Box::new(db_size_limit_bytes.clone()))
            .unwrap();
        registry.register(Box::new(db_over_limit.clone())).unwrap();
        registry.register(Box::new(queries_total.clone())).unwrap();
        registry
            .register(Box::new(queries_read_only_enforced.clone()))
            .unwrap();

        Self {
            registry,
            client_connections_total,
            client_connections_active,
            server_connections_total,
            server_connections_active,
            server_connection_errors_total,
            pool_hits_total,
            pool_misses_total,
            pool_timeouts_total,
            db_size_bytes,
            db_size_limit_bytes,
            db_over_limit,
            queries_total,
            queries_read_only_enforced,
        }
    }
}
