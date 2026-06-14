use std::time::{Duration, Instant};

pub(crate) const USER_AGENT: &str = concat!(
    "kyoku/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/kyoku-project/kyoku)"
);

pub(crate) fn blocking_client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .expect("failed to create HTTP client")
}

pub(crate) struct Throttle {
    rate_limit: Duration,
    last_request: Option<Instant>,
}

impl Throttle {
    pub(crate) fn new(rate_limit_ms: u64) -> Self {
        Self {
            rate_limit: Duration::from_millis(rate_limit_ms),
            last_request: None,
        }
    }

    pub(crate) fn wait(&mut self) {
        if let Some(last) = self.last_request {
            let elapsed = last.elapsed();
            if elapsed < self.rate_limit {
                std::thread::sleep(self.rate_limit - elapsed);
            }
        }
        self.last_request = Some(Instant::now());
    }
}

pub(crate) enum AttemptError {
    Retryable(String),
    Permanent(String),
}

/// Flatten an error and its `.source()` chain into a single colon-separated
/// string. reqwest's top-level Display is often just "error sending request",
/// with the actual cause (DNS, TLS, timeout) hiding one or two levels down.
pub(crate) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}
