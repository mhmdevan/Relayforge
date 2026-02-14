use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
pub struct TenantRateLimiter {
    capacity: f64,
    refill_per_second: f64,
    state: Arc<Mutex<HashMap<Uuid, TenantBucket>>>,
}

#[derive(Debug)]
struct TenantBucket {
    available_tokens: f64,
    last_refill: Instant,
}

impl TenantRateLimiter {
    pub fn new(refill_per_second: u32, burst: u32) -> Self {
        let capacity = burst.max(1) as f64;
        let refill_per_second = refill_per_second.max(1) as f64;
        Self {
            capacity,
            refill_per_second,
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn try_acquire(&self, tenant_id: Uuid) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();

        let bucket = state.entry(tenant_id).or_insert(TenantBucket {
            available_tokens: self.capacity,
            last_refill: now,
        });

        self.refill_bucket(bucket, now);

        if bucket.available_tokens >= 1.0 {
            bucket.available_tokens -= 1.0;
            return true;
        }

        false
    }

    fn refill_bucket(&self, bucket: &mut TenantBucket, now: Instant) {
        let elapsed = now.duration_since(bucket.last_refill);
        let refill_amount = elapsed.as_secs_f64() * self.refill_per_second;
        bucket.available_tokens = (bucket.available_tokens + refill_amount).min(self.capacity);
        bucket.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn tenant_limiter_isolated_per_tenant() {
        let limiter = TenantRateLimiter::new(1, 1);
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();

        assert!(limiter.try_acquire(t1).await);
        assert!(limiter.try_acquire(t2).await);
        assert!(!limiter.try_acquire(t1).await);
        assert!(!limiter.try_acquire(t2).await);

        tokio::time::sleep(Duration::from_millis(1200)).await;

        assert!(limiter.try_acquire(t1).await);
        assert!(limiter.try_acquire(t2).await);
    }
}
