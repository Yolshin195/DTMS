use anyhow::Result;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::attribute::{SERVICE_NAME, SERVICE_VERSION};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{filter::EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::AppConfig;

pub fn init_telemetry(cfg: &AppConfig) -> Result<SdkTracerProvider> {
    let resource = Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new(SERVICE_NAME, cfg.service_name.clone()))
        .with_attribute(opentelemetry::KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")))
        .build();

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&cfg.otlp_endpoint)
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::AlwaysOn)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .build();

    global::set_tracer_provider(provider.clone());
    let tracer = provider.tracer(cfg.service_name.clone());

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json().with_target(true).with_file(true).with_line_number(true))
        .with(OpenTelemetryLayer::new(tracer))
        .try_init()?;

    Ok(provider)
}