pub mod backend;
pub mod frontend;
pub mod messages;

pub use frontend::handle_client;

/// Pool key: uniquely identifies a backend connection pool.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct PoolKey {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
}

impl std::fmt::Display for PoolKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}/{}@{}",
            self.host, self.port, self.database, self.user
        )
    }
}

/// Information extracted from the client's startup message.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClientStartupInfo {
    pub user: String,
    pub database: String,
    pub password: String,
    pub max_db_size: Option<u64>,
    pub application_name: String,
    pub extra_params: Vec<(String, String)>,
}
