use anyhow::anyhow;
use opentelemetry::{KeyValue, global};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{
    LogExporter, MetricExporter, SpanExporter, WithExportConfig, WithTonicConfig,
};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracerProvider, Tracer};
use tonic::metadata::MetadataMap;
use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::filter::dynamic_filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

use super::metrics::init_metrics;
use crate::core::blueprint::telemetry::{OtlpExporter, Telemetry, TelemetryExporter};
use crate::core::runtime::TargetRuntime;
use crate::core::tracing::{
    default_tracing, default_tracing_gqlforge, get_log_level, gqlforge_filter_target,
};

static RESOURCE: std::sync::LazyLock<Resource> = std::sync::LazyLock::new(|| {
    Resource::builder()
        .with_service_name("gqlforge")
        .with_attributes([KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
            option_env!("APP_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
        )])
        .build()
});

fn set_trace_provider(
    exporter: &TelemetryExporter,
) -> anyhow::Result<Option<OpenTelemetryLayer<Registry, Tracer>>> {
    let provider = match exporter {
        TelemetryExporter::Stdout(_config) => {
            let exporter = opentelemetry_stdout::SpanExporter::default();
            SdkTracerProvider::builder()
                .with_simple_exporter(exporter)
                .with_resource(RESOURCE.clone())
                .build()
        }
        TelemetryExporter::Otlp(config) => {
            let exporter = build_otlp_span_exporter(config)?;
            SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(RESOURCE.clone())
                .build()
        }
        // Prometheus works only with metrics
        TelemetryExporter::Prometheus(_) => return Ok(None),
    };

    // Set the global tracer provider for context propagation
    global::set_tracer_provider(provider);

    // TEMPORARY: Disable tracing-opentelemetry layer due to version incompatibility
    // tracing-opentelemetry v0.32.1 depends on opentelemetry v0.31.0, but we use v0.32.0
    // This should be re-enabled once tracing-opentelemetry v0.33+ is released with
    // opentelemetry v0.32 support
    // TODO: Re-enable when tracing-opentelemetry supports opentelemetry v0.32
    Ok(None)
}

fn set_logger_provider(
    exporter: &TelemetryExporter,
) -> anyhow::Result<Option<OpenTelemetryTracingBridge<SdkLoggerProvider, SdkLogger>>> {
    let provider = match exporter {
        TelemetryExporter::Stdout(_config) => {
            let exporter = opentelemetry_stdout::LogExporter::default();
            SdkLoggerProvider::builder()
                .with_simple_exporter(exporter)
                .with_resource(RESOURCE.clone())
                .build()
        }
        TelemetryExporter::Otlp(config) => {
            let exporter = build_otlp_log_exporter(config)?;
            SdkLoggerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(RESOURCE.clone())
                .build()
        }
        // Prometheus works only with metrics
        TelemetryExporter::Prometheus(_) => return Ok(None),
    };

    let otel_tracing_appender = OpenTelemetryTracingBridge::new(&provider);

    Ok(Some(otel_tracing_appender))
}

fn set_meter_provider(exporter: &TelemetryExporter) -> anyhow::Result<()> {
    let provider = match exporter {
        TelemetryExporter::Stdout(_config) => {
            let exporter = opentelemetry_stdout::MetricExporter::default();
            let reader = PeriodicReader::builder(exporter).build();
            SdkMeterProvider::builder()
                .with_reader(reader)
                .with_resource(RESOURCE.clone())
                .build()
        }
        TelemetryExporter::Otlp(config) => {
            let exporter = build_otlp_metric_exporter(config)?;
            SdkMeterProvider::builder()
                .with_periodic_exporter(exporter)
                .with_resource(RESOURCE.clone())
                .build()
        }
        TelemetryExporter::Prometheus(_) => {
            let exporter = opentelemetry_prometheus::exporter()
                .with_registry(prometheus::default_registry().clone())
                .build()?;

            SdkMeterProvider::builder()
                .with_resource(RESOURCE.clone())
                .with_reader(exporter)
                .build()
        }
    };

    global::set_meter_provider(provider);

    Ok(())
}

fn build_otlp_span_exporter(
    config: &OtlpExporter,
) -> anyhow::Result<opentelemetry_otlp::SpanExporter> {
    SpanExporter::builder()
        .with_tonic()
        .with_endpoint(config.url.as_str())
        .with_metadata(MetadataMap::from_headers(config.headers.clone()))
        .build()
        .map_err(|e| anyhow!("Failed to create OTLP span exporter: {e}"))
}

fn build_otlp_log_exporter(
    config: &OtlpExporter,
) -> anyhow::Result<opentelemetry_otlp::LogExporter> {
    LogExporter::builder()
        .with_tonic()
        .with_endpoint(config.url.as_str())
        .with_metadata(MetadataMap::from_headers(config.headers.clone()))
        .build()
        .map_err(|e| anyhow!("Failed to create OTLP log exporter: {e}"))
}

fn build_otlp_metric_exporter(
    config: &OtlpExporter,
) -> anyhow::Result<opentelemetry_otlp::MetricExporter> {
    MetricExporter::builder()
        .with_tonic()
        .with_endpoint(config.url.as_str())
        .with_metadata(MetadataMap::from_headers(config.headers.clone()))
        .build()
        .map_err(|e| anyhow!("Failed to create OTLP metric exporter: {e}"))
}

fn set_tracing_subscriber(subscriber: impl Subscriber + Send + Sync) {
    // ignore errors since there is only one possible error when the global
    // subscriber is already set. The init is called multiple times in the same
    // process inside tests, so we want to ignore if it is already set
    let _ = tracing::subscriber::set_global_default(subscriber);
}

///
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn init_opentelemetry(config: Telemetry, runtime: &TargetRuntime) -> anyhow::Result<()> {
    if let Some(export) = &config.export {
        let _trace_layer = set_trace_provider(export)?;
        let log_layer = set_logger_provider(export)?;
        set_meter_provider(export)?;

        global::set_text_map_propagator(TraceContextPropagator::new());

        // TEMPORARY: Build subscriber without trace_layer due to version incompatibility
        // Once tracing-opentelemetry v0.33+ is released with opentelemetry v0.32 support,
        // uncomment the trace_layer line below
        let subscriber = tracing_subscriber::registry()
            // .with(trace_layer)  // TODO: Re-enable when tracing-opentelemetry supports opentelemetry v0.32
            .with(default_tracing())
            .with(
                log_layer.with_filter(dynamic_filter_fn(|_metatada, context| {
                    // ignore logs that are generated inside tracing::Span since they will be logged
                    // anyway with tracer_provider and log here only the events without associated
                    // span
                    context.lookup_current().is_none()
                })),
            )
            .with(gqlforge_filter_target())
            .with(get_log_level().unwrap_or(LevelFilter::INFO));

        init_metrics(runtime).await?;

        set_tracing_subscriber(subscriber);
    } else {
        set_tracing_subscriber(default_tracing_gqlforge());
    }

    Ok(())
}
