use anyhow::Context;
use once_cell::sync::Lazy;
use prometheus::{
    register_histogram_vec, register_int_counter, register_int_counter_vec, register_int_gauge,
    Encoder, HistogramVec, IntCounter, IntCounterVec, IntGauge, TextEncoder,
};
use std::time::Duration;

static HTTP_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "http_requests_total",
        "Total number of HTTP requests",
        &["route", "method", "status"]
    )
    .expect("register http_requests_total")
});

static HTTP_REQUEST_DURATION_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "http_request_duration_seconds",
        "HTTP request duration in seconds",
        &["route", "method"],
        vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
    .expect("register http_request_duration_seconds")
});

static JOBS_CREATED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "jobs_created_total",
        "Total number of created jobs",
        &["source"]
    )
    .expect("register jobs_created_total")
});

static JOBS_PROCESSED_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "jobs_processed_total",
        "Total number of processed jobs",
        &["status"]
    )
    .expect("register jobs_processed_total")
});

static JOB_PROCESSING_DURATION_SECONDS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "job_processing_duration_seconds",
        "Job processing duration in seconds",
        &["status"],
        vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]
    )
    .expect("register job_processing_duration_seconds")
});

static JOB_RETRIES_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!("job_retries_total", "Total number of scheduled job retries")
        .expect("register job_retries_total")
});

static DLQ_MESSAGES_TOTAL: Lazy<IntCounter> = Lazy::new(|| {
    register_int_counter!(
        "dlq_messages_total",
        "Total number of dead-lettered jobs/messages"
    )
    .expect("register dlq_messages_total")
});

static OUTBOX_PENDING_COUNT: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!(
        "outbox_pending_count",
        "Number of outbox messages in pending state"
    )
    .expect("register outbox_pending_count")
});

fn normalize_label<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn ensure_registered() {
    Lazy::force(&HTTP_REQUESTS_TOTAL);
    Lazy::force(&HTTP_REQUEST_DURATION_SECONDS);
    Lazy::force(&JOBS_CREATED_TOTAL);
    Lazy::force(&JOBS_PROCESSED_TOTAL);
    Lazy::force(&JOB_PROCESSING_DURATION_SECONDS);
    Lazy::force(&JOB_RETRIES_TOTAL);
    Lazy::force(&DLQ_MESSAGES_TOTAL);
    Lazy::force(&OUTBOX_PENDING_COUNT);
}

pub fn record_http_request(route: &str, method: &str, status: u16, duration: Duration) {
    ensure_registered();
    let route = normalize_label(route, "unknown");
    let method = normalize_label(method, "unknown");
    let status = status.to_string();

    HTTP_REQUESTS_TOTAL
        .with_label_values(&[route, method, status.as_str()])
        .inc();
    HTTP_REQUEST_DURATION_SECONDS
        .with_label_values(&[route, method])
        .observe(duration.as_secs_f64());
}

pub fn inc_jobs_created(source: &str) {
    ensure_registered();
    let source = normalize_label(source, "unknown");
    JOBS_CREATED_TOTAL.with_label_values(&[source]).inc();
}

pub fn inc_jobs_processed(status: &str) {
    ensure_registered();
    let status = normalize_label(status, "unknown");
    JOBS_PROCESSED_TOTAL.with_label_values(&[status]).inc();
}

pub fn observe_job_processing_duration(status: &str, duration: Duration) {
    ensure_registered();
    let status = normalize_label(status, "unknown");
    JOB_PROCESSING_DURATION_SECONDS
        .with_label_values(&[status])
        .observe(duration.as_secs_f64());
}

pub fn inc_job_retries() {
    ensure_registered();
    JOB_RETRIES_TOTAL.inc();
}

pub fn inc_dlq_messages() {
    ensure_registered();
    DLQ_MESSAGES_TOTAL.inc();
}

pub fn set_outbox_pending_count(count: i64) {
    ensure_registered();
    let clamped = count.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    OUTBOX_PENDING_COUNT.set(clamped as i64);
}

pub fn render_metrics() -> anyhow::Result<String> {
    ensure_registered();
    let metric_families = prometheus::gather();
    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    encoder
        .encode(&metric_families, &mut buf)
        .context("failed to encode prometheus metrics")?;
    String::from_utf8(buf).context("prometheus output is not valid utf-8")
}
