pub mod admin;
pub mod admin_auth;
pub mod connectors;
pub mod docs;
pub mod hardening;
pub mod jobs;
pub mod tenant_limits;
pub mod tenant_secrets;
pub mod webhooks;

use axum::{
    body::Body,
    extract::MatchedPath,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use infra::db::{self, PgPool};
use serde::Serialize;
use std::{collections::HashMap, time::Instant};
use tower_http::{
    catch_panic::CatchPanicLayer,
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub started_at: Instant,
    pub pool: PgPool,
    pub default_tenant_id: Uuid,
    pub connector_registry: connectors::ConnectorRegistry,
    pub tenant_secret_store: tenant_secrets::TenantSecretStore,
    pub admin_access_control: admin_auth::AdminAccessControl,
    pub tenant_rate_limiter: tenant_limits::TenantRateLimiter,
    pub tenant_daily_quota: i64,
    pub stripe_signature_tolerance_seconds: i64,
    pub ingress_rate_limiter: hardening::IngressRateLimiter,
    pub max_body_bytes: usize,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_seconds: u64,
}

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            uptime_seconds: uptime,
        }),
    )
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match db::ping(&state.pool).await {
        Ok(_) => (StatusCode::OK, "ready"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "not_ready"),
    }
}

async fn metrics_handler() -> Response {
    match common::metrics::render_metrics() {
        Ok(body) => {
            let mut response = Response::new(body.into());
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            response
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render metrics: {err}"),
        )
            .into_response(),
    }
}

async fn metrics_middleware(req: Request<Body>, next: Next) -> Response {
    let started_at = std::time::Instant::now();
    let method = req.method().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_else(|| req.uri().path())
        .to_string();

    let response = next.run(req).await;
    common::metrics::record_http_request(
        &route,
        &method,
        response.status().as_u16(),
        started_at.elapsed(),
    );

    response
}

fn request_trace_carrier(headers: &HeaderMap) -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    for key in ["traceparent", "tracestate", "baggage"] {
        if let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) {
            carrier.insert(key.to_string(), value.to_string());
        }
    }
    carrier
}

async fn otel_parent_middleware(req: Request<Body>, next: Next) -> Response {
    let carrier = request_trace_carrier(req.headers());
    if !carrier.is_empty() {
        let span = tracing::Span::current();
        common::observability::set_parent_from_carrier(&span, &carrier);
    }

    next.run(req).await
}

pub fn build_app(state: AppState) -> Router {
    let ingress_rate_limiter = state.ingress_rate_limiter.clone();
    let max_body_bytes = state.max_body_bytes;

    let admin_router = Router::new()
        .route("/jobs/:id/reprocess", post(admin::reprocess_job))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth::require_operator_or_admin,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_handler))
        .route("/openapi.json", get(docs::openapi_json))
        .route("/docs", get(docs::docs_redirect))
        .route("/docs/", get(docs::swagger_ui))
        .route("/webhooks/:source", post(webhooks::handle_webhook))
        .route("/jobs/:id", get(jobs::get_job))
        .nest("/admin", admin_router)
        .with_state(state)
        .layer(middleware::from_fn(metrics_middleware))
        .layer(middleware::from_fn(otel_parent_middleware))
        .layer(middleware::from_fn_with_state(
            ingress_rate_limiter,
            hardening::enforce_ingress_rate_limit,
        ))
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
}
