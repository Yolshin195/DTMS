use anyhow::Result;
use opentelemetry::global;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    runtime,
    trace::{self, RandomIdGenerator, Sampler},
    Resource,
};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    filter::EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::config::AppConfig;

/// Initialise the global tracing subscriber:
/// - JSON structured logs to stdout
/// - OpenTelemetry trace export to OTLP/Jaeger
pub fn init_telemetry(cfg: &AppConfig) -> Result<()> {
    // ── OTLP tracer ──────────────────────────────────────────────────────────
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&cfg.otlp_endpoint),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(Resource::new(vec![
                    opentelemetry::KeyValue::new(SERVICE_NAME, cfg.service_name.clone()),
                    opentelemetry::KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                ])),
        )
        .install_batch(runtime::Tokio)?;

    // ── Env filter (e.g. RUST_LOG=task_service=debug,info) ──────────────────
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    // ── JSON log layer ───────────────────────────────────────────────────────
    let fmt_layer = fmt::layer()
        .json()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);

    // ── OTel span layer ──────────────────────────────────────────────────────
    let otel_layer = OpenTelemetryLayer::new(tracer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .try_init()?;

    Ok(())
}

/// Flush all pending spans before process exit.
pub fn shutdown_telemetry() {
    global::shutdown_tracer_provider();
}
