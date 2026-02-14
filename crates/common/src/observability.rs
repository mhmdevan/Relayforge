use crate::config::LogSettings;
use anyhow::Context;
use opentelemetry::{
    global,
    propagation::{Extractor, Injector},
    trace::{TraceContextExt, TracerProvider as _},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace as sdktrace, Resource};
use std::collections::HashMap;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init_tracing(service_name: &str, log: &LogSettings) -> anyhow::Result<()> {
    // Prefer RUST_LOG if set; otherwise use config fallback.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&log.filter))
        .context("failed to create EnvFilter")?;

    global::set_text_map_propagator(TraceContextPropagator::new());

    if parse_env_bool("OTEL__ENABLED", true) {
        let tracer = build_otlp_tracer(service_name)?;

        if log.json {
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer.clone());
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .json()
                        .with_current_span(true)
                        .with_span_list(true),
                )
                .with(otel_layer)
                .init();
        } else {
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_target(true))
                .with(otel_layer)
                .init();
        }
    } else if log.json {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(true))
            .init();
    }

    Ok(())
}

fn parse_env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

fn build_otlp_tracer(service_name: &str) -> anyhow::Result<sdktrace::Tracer> {
    let otlp_endpoint = std::env::var("OTEL__EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:4317".to_string());
    let otlp_timeout_ms: u64 = std::env::var("OTEL__EXPORTER_OTLP_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(otlp_endpoint)
        .with_timeout(std::time::Duration::from_millis(otlp_timeout_ms));

    let resource = Resource::new(vec![KeyValue::new(
        "service.name",
        service_name.to_string(),
    )]);
    let trace_config = sdktrace::Config::default()
        .with_sampler(sdktrace::Sampler::AlwaysOn)
        .with_resource(resource);

    let provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .with_trace_config(trace_config)
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .context("failed to initialize OTLP tracing pipeline")?;

    Ok(provider.tracer(service_name.to_string()))
}

pub fn inject_current_context(carrier: &mut HashMap<String, String>) {
    let cx = Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut HashMapInjector::new(carrier));
    });
}

pub fn set_parent_from_carrier(span: &Span, carrier: &HashMap<String, String>) {
    let parent = extract_context_from_carrier(carrier);
    if parent.span().span_context().is_valid() {
        span.set_parent(parent);
    }
}

pub fn extract_context_from_carrier(carrier: &HashMap<String, String>) -> opentelemetry::Context {
    global::get_text_map_propagator(|propagator| {
        propagator.extract(&HashMapExtractor::new(carrier))
    })
}

pub fn current_trace_id() -> Option<String> {
    let cx = Span::current().context();
    let span_ctx = cx.span().span_context().clone();
    if span_ctx.is_valid() {
        Some(span_ctx.trace_id().to_string())
    } else {
        None
    }
}

struct HashMapExtractor<'a> {
    values: &'a HashMap<String, String>,
}

impl<'a> HashMapExtractor<'a> {
    fn new(values: &'a HashMap<String, String>) -> Self {
        Self { values }
    }
}

impl Extractor for HashMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .or_else(|| self.values.get(&key.to_ascii_lowercase()))
            .map(String::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.values.keys().map(String::as_str).collect()
    }
}

struct HashMapInjector<'a> {
    values: &'a mut HashMap<String, String>,
}

impl<'a> HashMapInjector<'a> {
    fn new(values: &'a mut HashMap<String, String>) -> Self {
        Self { values }
    }
}

impl Injector for HashMapInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.values.insert(key.to_string(), value);
    }
}
