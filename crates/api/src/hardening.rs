use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Clone)]
pub struct IngressRateLimiter {
    capacity: f64,
    refill_per_second: f64,
    bucket: Arc<Mutex<TokenBucket>>,
}

#[derive(Debug)]
struct TokenBucket {
    available_tokens: f64,
    last_refill: Instant,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

impl IngressRateLimiter {
    pub fn new(refill_per_second: u32, burst: u32) -> Self {
        let capacity = burst.max(1) as f64;
        let refill_per_second = refill_per_second.max(1) as f64;
        Self {
            capacity,
            refill_per_second,
            bucket: Arc::new(Mutex::new(TokenBucket {
                available_tokens: capacity,
                last_refill: Instant::now(),
            })),
        }
    }

    pub async fn try_acquire(&self) -> bool {
        let mut bucket = self.bucket.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill);
        self.refill_locked(&mut bucket, elapsed, now);

        if bucket.available_tokens >= 1.0 {
            bucket.available_tokens -= 1.0;
            return true;
        }

        false
    }

    fn refill_locked(&self, bucket: &mut TokenBucket, elapsed: Duration, now: Instant) {
        let refill_amount = elapsed.as_secs_f64() * self.refill_per_second;
        bucket.available_tokens = (bucket.available_tokens + refill_amount).min(self.capacity);
        bucket.last_refill = now;
    }
}

pub async fn enforce_ingress_rate_limit(
    State(rate_limiter): State<IngressRateLimiter>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !req.uri().path().starts_with("/webhooks/") {
        return next.run(req).await;
    }

    if rate_limiter.try_acquire().await {
        return next.run(req).await;
    }

    warn!(path = %req.uri().path(), "ingress rate limit exceeded");
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(ErrorBody {
            error: "rate limit exceeded".to_string(),
        }),
    )
        .into_response()
}
