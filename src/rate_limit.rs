use crate::util::{extract_client_ip, json_error};
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::{body::Body, http::Request, response::Response};
use std::net::SocketAddr as ClientAddr;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tokio::sync::RwLock;
use tower::{Layer, Service};

struct RateLimitConfig {
    capacity: u32,
    refill_per_second: u32,
}
#[derive(Clone, Debug)]
struct RateBucket {
    tokens: f64,
    last: Instant,
}

#[derive(Clone)]
pub struct RateLimiterInner {
    buckets: Arc<RwLock<HashMap<String, RateBucket>>>,
    cfg: Arc<RateLimitConfig>,
}
impl RateLimiterInner {
    pub fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self {
            buckets: Arc::new(RwLock::new(HashMap::new())),
            cfg: Arc::new(RateLimitConfig {
                capacity,
                refill_per_second,
            }),
        }
    }
    pub async fn check(&self, ip: &str) -> bool {
        let mut map = self.buckets.write().await;
        let entry = map.entry(ip.to_string()).or_insert(RateBucket {
            tokens: self.cfg.capacity as f64,
            last: Instant::now(),
        });
        let now = Instant::now();
        let elapsed = now.duration_since(entry.last).as_secs_f64();
        if elapsed > 0.0 {
            let refill = elapsed * self.cfg.refill_per_second as f64;
            entry.tokens = (entry.tokens + refill).min(self.cfg.capacity as f64);
            entry.last = now;
        }
        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
pub struct RateLimitLayer {
    limiter: RateLimiterInner,
}
impl RateLimitLayer {
    pub fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self {
            limiter: RateLimiterInner::new(capacity, refill_per_second),
        }
    }
}
impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            inner,
            limiter: self.limiter.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    limiter: RateLimiterInner,
}
impl<S> Service<Request<Body>> for RateLimitService<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Error: std::fmt::Display,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }
    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let limiter = self.limiter.clone();
        let mut inner = self.inner.clone();
        let path = req.uri().path().to_string();
        // Bypass rate limiting for core static assets (css/js) so ban page renders correctly
        if path.starts_with("/css/") || path.starts_with("/js/") {
            return Box::pin(async move { inner.call(req).await });
        }
        let edge_ip = req
            .extensions()
            .get::<ConnectInfo<ClientAddr>>()
            .map(|c| c.0.ip());
        let header_ip = {
            let h = req.headers();
            extract_client_ip(h, edge_ip)
        };
        Box::pin(async move {
            if !limiter.check(&header_ip).await {
                return Ok(json_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "slow down",
                ));
            }
            inner.call(req).await
        })
    }
}

pub fn build_rate_limiter() -> RateLimitLayer {
    // Less strict: allow 180 requests burst, refill 3 tokens/sec
    RateLimitLayer::new(180, 3)
}
