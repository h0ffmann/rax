//! Retrying Substrate calls that fail with retryable codes.

use std::future::Future;
use std::time::Duration;

use rand::Rng;
use tonic::{Code, Status};

/// How the client retries and polls. The defaults match AX: up to 10 calls
/// with jittered exponential backoff from 200ms, doubling, capped at 2s, for
/// roughly 6-20s in total.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub initial_interval: Duration,
    pub max_interval: Duration,
    pub multiplier: f64,
    /// How often to check whether a deleted actor is gone.
    pub poll_interval: Duration,
    /// How long to wait for a deleted actor to be gone.
    pub delete_timeout: Duration,
    /// How long, and how often, to retry creating an actor while a previous
    /// one with the same name is still being deleted.
    pub deleting_wait_attempts: u32,
    pub deleting_wait_interval: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 10,
            initial_interval: Duration::from_millis(200),
            max_interval: Duration::from_secs(2),
            multiplier: 2.0,
            poll_interval: Duration::from_millis(200),
            delete_timeout: Duration::from_secs(120),
            deleting_wait_attempts: 20,
            deleting_wait_interval: Duration::from_millis(500),
        }
    }
}

impl RetryPolicy {
    /// A policy with tiny intervals, for tests.
    pub fn fast() -> Self {
        Self {
            initial_interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(5),
            poll_interval: Duration::from_millis(5),
            delete_timeout: Duration::from_secs(5),
            deleting_wait_interval: Duration::from_millis(5),
            ..Self::default()
        }
    }

    /// Calls `f` until it succeeds, fails with a code not in `retryable`, or
    /// the attempts run out. Between attempts it waits with ±50% jitter.
    pub async fn run<T, F, Fut>(&self, retryable: &[Code], mut f: F) -> Result<T, Status>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, Status>>,
    {
        let mut interval = self.initial_interval;
        let mut attempt = 1;
        loop {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) if attempt >= self.attempts || !retryable.contains(&e.code()) => {
                    return Err(e);
                }
                Err(_) => {}
            }
            let jitter = rand::rng().random_range(0.5..1.5);
            tokio::time::sleep(interval.mul_f64(jitter)).await;
            interval = interval.mul_f64(self.multiplier).min(self.max_interval);
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn calls_until(
        policy: &RetryPolicy,
        retryable: &[Code],
        fail: impl Fn(u32) -> Option<Code>,
    ) -> (u32, Result<(), Status>) {
        let calls = AtomicU32::new(0);
        let res = policy
            .run(retryable, || {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let out = fail(n).map_or(Ok(()), |c| Err(Status::new(c, format!("call {n}"))));
                async move { out }
            })
            .await;
        (calls.load(Ordering::SeqCst), res)
    }

    #[tokio::test]
    async fn retries_listed_codes_until_success() {
        let p = RetryPolicy::fast();
        let (n, res) = calls_until(&p, &[Code::Unavailable], |n| {
            (n < 3).then_some(Code::Unavailable)
        })
        .await;
        assert_eq!(n, 3);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn stops_on_other_codes() {
        let p = RetryPolicy::fast();
        let (n, res) =
            calls_until(&p, &[Code::Unavailable], |_| Some(Code::ResourceExhausted)).await;
        assert_eq!(n, 1);
        assert_eq!(res.unwrap_err().code(), Code::ResourceExhausted);
    }

    #[tokio::test]
    async fn gives_up_after_the_attempt_limit() {
        let p = RetryPolicy {
            attempts: 4,
            ..RetryPolicy::fast()
        };
        let (n, res) = calls_until(&p, &[Code::Aborted, Code::Unavailable], |_| {
            Some(Code::Aborted)
        })
        .await;
        assert_eq!(n, 4);
        let e = res.unwrap_err();
        assert_eq!(e.code(), Code::Aborted);
        assert_eq!(e.message(), "call 4", "the last error is returned");
    }

    #[test]
    fn transient_errors() {
        use crate::Error;
        for (code, want) in [
            (Code::Unavailable, true),
            (Code::DeadlineExceeded, true),
            (Code::Aborted, false),
            (Code::InvalidArgument, false),
            (Code::NotFound, false),
        ] {
            let e = Error::new("doing a thing", Status::new(code, "x")).context("outer");
            assert_eq!(e.is_transient(), want, "{code:?}");
            assert_eq!(e.to_string(), "outer: doing a thing: x");
            assert_eq!(e.to_status().code(), code);
        }
    }
}
