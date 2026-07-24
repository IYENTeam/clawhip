use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: u32,
    tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    #[must_use]
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            tokens: f64::from(capacity),
            refill_rate: refill_per_sec,
            last_refill: Instant::now(),
        }
    }

    pub fn consume_or_delay(&mut self, count: u32) -> Duration {
        self.refill();
        let needed = f64::from(count);
        if self.tokens >= needed {
            self.tokens -= needed;
            Duration::ZERO
        } else if self.refill_rate <= f64::EPSILON {
            Duration::from_secs(1)
        } else {
            let missing = needed - self.tokens;
            self.tokens = 0.0;
            Duration::from_secs_f64(missing / self.refill_rate)
        }
    }

    /// Attempts to consume tokens without taking any available partial refill on rejection.
    pub fn try_consume(&mut self, count: u32) -> bool {
        self.refill();
        let needed = f64::from(count);
        if self.tokens < needed {
            return false;
        }
        self.tokens -= needed;
        true
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = elapsed
            .mul_add(self.refill_rate, self.tokens)
            .min(f64::from(self.capacity));
        self.last_refill = now;
    }
}

#[derive(Debug, Clone)]
pub struct RateLimiter {
    buckets: HashMap<String, TokenBucket>,
    capacity: u32,
    refill_per_sec: f64,
}

impl RateLimiter {
    #[must_use]
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_per_sec,
        }
    }

    pub fn delay_for(&mut self, key: &str) -> Duration {
        let capacity = self.capacity;
        let refill_per_sec = self.refill_per_sec;
        self.buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(capacity, refill_per_sec))
            .consume_or_delay(1)
    }

    /// Consumes one token for `key`, leaving the bucket untouched when unavailable.
    pub fn try_consume(&mut self, key: &str) -> bool {
        let capacity = self.capacity;
        let refill_per_sec = self.refill_per_sec;
        self.buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(capacity, refill_per_sec))
            .try_consume(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_allows_up_to_capacity_without_delay() {
        let mut bucket = TokenBucket::new(2, 0.0);
        assert_eq!(bucket.consume_or_delay(1), Duration::ZERO);
        assert_eq!(bucket.consume_or_delay(1), Duration::ZERO);
        assert!(bucket.consume_or_delay(1) >= Duration::from_secs(1));
    }

    #[test]
    fn limiter_is_scoped_per_key() {
        let mut limiter = RateLimiter::new(1, 0.0);
        assert_eq!(limiter.delay_for("a"), Duration::ZERO);
        assert!(limiter.delay_for("a") >= Duration::from_secs(1));
        assert_eq!(limiter.delay_for("b"), Duration::ZERO);
    }

    #[test]
    fn rejected_try_consume_keeps_partial_refill_available() {
        let mut bucket = TokenBucket::new(1, 0.0);
        bucket.tokens = 0.5;

        assert!(!bucket.try_consume(1));
        assert_eq!(bucket.tokens, 0.5);
    }

    #[test]
    fn limiter_try_consume_is_scoped_per_key() {
        let mut limiter = RateLimiter::new(1, 0.0);
        assert!(limiter.try_consume("a"));
        assert!(!limiter.try_consume("a"));
        assert!(limiter.try_consume("b"));
    }
}
