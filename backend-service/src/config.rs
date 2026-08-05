/// Service configuration loaded from environment variables.
#[derive(Clone)]
pub struct Config {
    pub port: u16,
    /// Neon/Postgres connection string.
    pub database_url: String,
    /// Shared secret clients must send in `X-Api-Key`. Empty string disables auth (dev only).
    pub api_secret: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            database_url: std::env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            api_secret: std::env::var("API_SECRET").unwrap_or_default(),
        }
    }
}
