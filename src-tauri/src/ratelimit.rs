// SPDX-License-Identifier: AGPL-3.0-only

//! Sliding-window rate limits for the IPC commands. The remote page shares the main
//! world with the injected script, so a misbehaving or compromised page script could
//! call the commands too; these limits bound what it can provoke.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct RateLimit {
    window: Duration,
    max: usize,
    hits: VecDeque<Instant>,
}

impl RateLimit {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            window,
            max,
            hits: VecDeque::with_capacity(max),
        }
    }

    /// Records a call; false if the limit for the current window is exhausted.
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .hits
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.hits.pop_front();
        }
        if self.hits.len() >= self.max {
            return false;
        }
        self.hits.push_back(now);
        true
    }
}

/// Limits per command. Normal operation stays far below them: the script reports on
/// change (throttled to 1/s) plus every 30–60 s.
#[derive(Debug)]
pub struct Limits {
    pub config: RateLimit,
    pub report: RateLimit,
    pub notify: RateLimit,
    pub zoom: RateLimit,
}

impl Default for Limits {
    fn default() -> Self {
        let s = Duration::from_secs;
        Self {
            config: RateLimit::new(30, s(60)),
            report: RateLimit::new(30, s(10)),
            notify: RateLimit::new(6, s(60)),
            zoom: RateLimit::new(30, s(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_window() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut r = RateLimit::new(2, s(10));
        assert!(r.allow(t0));
        assert!(r.allow(t0 + s(1)));
        assert!(!r.allow(t0 + s(2)));
        assert!(r.allow(t0 + s(10)));
        assert!(!r.allow(t0 + s(10)));
        assert!(r.allow(t0 + s(11)));
    }
}
