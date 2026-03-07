//! OTEL SDK initialisation: tracing subscriber + OTLP exporter via vsock.

use std::time::Duration;

use anyhow::{Context, Result};
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{runtime, Resource};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use super::log_writer::SharedTcpWriter;

/// Initialise the global tracing subscriber, OTLP tracing pipeline, and OTLP
/// metrics pipeline.
///
/// Configures:
/// - A JSON-formatted [`tracing_subscriber`] layer writing to stderr (local debug).
/// - A JSON-formatted [`tracing_subscriber`] layer writing to a TCP socket bridged
///   over vsock to the parent EC2 ADOT Collector's `tcplog` receiver → CloudWatch Logs.
/// - A [`tracing_opentelemetry`] layer exporting spans via OTLP to the collector.
/// - An OTLP metrics pipeline exporting to the same endpoint every 15 s (registered
///   as the global [`opentelemetry::global`] meter provider).
///
/// The `log_writer` is `None` when the log bridge TCP socket could not be connected
/// (e.g., ADOT Collector not yet running on parent); in that case only stderr is used.
///
/// # Errors
///
/// Returns an error if the OTLP exporter or tracing subscriber cannot be initialised.
pub fn init_telemetry(
    otlp_endpoint: &str,
    log_level: &str,
    log_writer: Option<SharedTcpWriter>,
) -> Result<()> {
    // --- Metrics pipeline ---
    let meter_provider = opentelemetry_otlp::new_pipeline()
        .metrics(runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint),
        )
        .with_resource(service_resource())
        .with_period(Duration::from_secs(15))
        .build()
        .context("failed to install OTLP metrics pipeline")?;
    opentelemetry::global::set_meter_provider(meter_provider);

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

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().json())
        .with(otel_layer);

    if let Some(writer) = log_writer {
        // Second JSON layer forwarding log records to the ADOT Collector tcplog receiver.
        registry
            .with(tracing_subscriber::fmt::layer().json().with_writer(writer))
            .try_init()
    } else {
        registry.try_init()
    }
    .context("failed to initialise tracing subscriber")
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
