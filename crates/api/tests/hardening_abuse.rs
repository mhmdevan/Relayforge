use api::hardening::{enforce_ingress_rate_limit, IngressRateLimiter};
use axum::{http::StatusCode, middleware, response::IntoResponse, routing::post, Router};
use reqwest::Client;
use std::time::Duration;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower_http::limit::RequestBodyLimitLayer;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burst_1000_requests_triggers_rate_limit() -> anyhow::Result<()> {
    let limiter = IngressRateLimiter::new(10, 10);
    let app = build_ingress_test_app(limiter, 1024 * 1024);
    let (server_task, shutdown_tx, base_url) = spawn_app(app).await?;

    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let mut join_set = tokio::task::JoinSet::new();
    for _ in 0..1000 {
        let client = client.clone();
        let url = format!("{}/webhooks/generic", base_url);
        join_set.spawn(async move {
            client
                .post(url)
                .header("content-type", "application/json")
                .body("{\"event\":\"burst\"}")
                .send()
                .await
                .map(|r| r.status())
        });
    }

    let mut accepted = 0usize;
    let mut limited = 0usize;
    let mut other = 0usize;

    while let Some(result) = join_set.join_next().await {
        let status = result??;
        if status == StatusCode::ACCEPTED {
            accepted += 1;
        } else if status == StatusCode::TOO_MANY_REQUESTS {
            limited += 1;
        } else {
            other += 1;
        }
    }

    cleanup(server_task, shutdown_tx).await;

    assert!(limited > 0, "expected at least one 429 response");
    assert_eq!(
        accepted + limited + other,
        1000,
        "unexpected response count"
    );
    assert_eq!(other, 0, "unexpected non-202/non-429 responses");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_payload_returns_413() -> anyhow::Result<()> {
    let limiter = IngressRateLimiter::new(50_000, 50_000);
    let app = build_ingress_test_app(limiter, 1024);
    let (server_task, shutdown_tx, base_url) = spawn_app(app).await?;

    let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    let oversized = vec![b'a'; 2048];
    let response = client
        .post(format!("{}/webhooks/generic", base_url))
        .header("content-type", "application/json")
        .body(oversized)
        .send()
        .await?;

    cleanup(server_task, shutdown_tx).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

fn build_ingress_test_app(rate_limiter: IngressRateLimiter, max_body_bytes: usize) -> Router {
    Router::new()
        .route("/webhooks/generic", post(accepted_handler))
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        .layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            enforce_ingress_rate_limit,
        ))
        .with_state(rate_limiter)
}

async fn accepted_handler() -> impl IntoResponse {
    StatusCode::ACCEPTED
}

async fn spawn_app(app: Router) -> anyhow::Result<(JoinHandle<()>, oneshot::Sender<()>, String)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok((task, shutdown_tx, format!("http://{}", addr)))
}

async fn cleanup(server_task: JoinHandle<()>, shutdown_tx: oneshot::Sender<()>) {
    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
