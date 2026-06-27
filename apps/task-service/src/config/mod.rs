use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    /// Bind address, e.g. "0.0.0.0:8080"
    #[serde(default = "default_host")]
    pub host: String,

    /// PostgreSQL connection URL
    pub database_url: String,

    /// Max DB connections in pool
    #[serde(default = "default_pool_size")]
    pub database_pool_size: u32,

    /// NATS connection URL, e.g. "nats://nats:4222"
    #[serde(default = "default_nats_url")]
    pub nats_url: String,

    /// OTLP collector endpoint, e.g. "http://otel-collector:4317"
    #[serde(default = "default_otlp")]
    pub otlp_endpoint: String,

    /// Log level string passed to EnvFilter
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Service name reported to OTel
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

fn default_host()         -> String { "0.0.0.0:8080".into() }
fn default_pool_size()    -> u32    { 10 }
fn default_nats_url()     -> String { "nats://localhost:4222".into() }
fn default_otlp()         -> String { "http://localhost:4317".into() }
fn default_log_level()    -> String { "info".into() }
fn default_service_name() -> String { "task-service".into() }

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        // Load .env if present (dev convenience)
        dotenvy::dotenv().ok();

        config::Config::builder()
            .add_source(config::Environment::default().separator("__"))
            .build()
            .context("failed to build config")?
            .try_deserialize()
            .context("failed to deserialize config")
    }
}
