//! OTEL SDK initialisation: tracing subscriber + OTLP exporter via vsock.

use anyhow::{Context, Result};
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{runtime, Resource};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialise the global tracing subscriber and OTEL pipeline.
///
/// Configures:
/// - A JSON-formatted [`tracing_subscriber`] layer for structured log output.
/// - A [`tracing_opentelemetry`] layer that exports spans to the OTLP endpoint.
/// - An OTLP metrics pipeline (counter + histogram).
///
/// # Errors
///
/// Returns an error if the OTLP exporter or SDK pipeline cannot be initialised.
pub fn init_telemetry(otlp_endpoint: &str, log_level: &str) -> Result<()> {
    // --- Tracing pipeline ---
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint),
        )
        .with_trace_config(
            opentelemetry_sdk::trace::Config::default().with_resource(service_resource()),
        )
        .install_batch(runtime::Tokio)
        .context("failed to install OTLP tracing pipeline")?;

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // --- Subscriber ---
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().json())
        .with(otel_layer)
        .try_init()
        .context("failed to initialise tracing subscriber")?;

    Ok(())
}

fn service_resource() -> Resource {
    Resource::new(vec![
        KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
            "nitro-enc-svc",
        ),
        KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
            env!("CARGO_PKG_VERSION"),
        ),
    ])
}
