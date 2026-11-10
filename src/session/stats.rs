//! What the session bar shows next to the resolution: the encoding in use and the round-trip time.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How often the UI sends a one-pixel update request to measure the round trip.
const PING_EVERY: Duration = Duration::from_secs(2);
/// The minimum of the last few samples: a reply that was already in flight can only read low,
/// and one stuck behind a large update only reads high, so the minimum is the best estimate.
const SAMPLES: usize = 5;

#[derive(Default)]
struct Inner {
    last_ping: Option<Instant>,
    pinged: Option<Instant>,
    rtts: VecDeque<Duration>,
    encoding: Option<&'static str>,
    /// Start of the current one-second window, the updates counted in it and the last full count.
    window: Option<Instant>,
    count: u32,
    fps: u32,
}

/// Written by the reader thread, read by the UI. There is no RFB ping, so the round trip is the time
/// from a non-incremental 1×1 `FramebufferUpdateRequest` to the next `FramebufferUpdate`, which the
/// server must send even when the screen is static.
#[derive(Default)]
pub struct Stats(Mutex<Inner>);

impl Stats {
    /// True once per interval; the caller then sends the request.
    pub fn ping_due(&self) -> bool {
        let mut s = self.0.lock().unwrap();
        if s.last_ping.is_some_and(|t| t.elapsed() < PING_EVERY) {
            return false;
        }
        let now = Instant::now();
        s.last_ping = Some(now);
        s.pinged = Some(now);
        true
    }

    /// Called for every `FramebufferUpdate`; `encoding` is the one used for its large rectangles.
    pub fn update_received(&self, encoding: Option<&'static str>) {
        let mut s = self.0.lock().unwrap();
        s.count += 1;
        let window = *s.window.get_or_insert_with(Instant::now);
        if window.elapsed() >= Duration::from_secs(1) {
            s.fps = s.count;
            s.count = 0;
            s.window = Some(Instant::now());
        }
        if let Some(t) = s.pinged.take() {
            if s.rtts.len() == SAMPLES {
                s.rtts.pop_front();
            }
            s.rtts.push_back(t.elapsed());
        }
        if encoding.is_some() {
            s.encoding = encoding;
        }
    }

    pub fn latency(&self) -> Option<Duration> {
        self.0.lock().unwrap().rtts.iter().min().copied()
    }

    /// Screen updates received in the last full second; 0 once they stop arriving.
    pub fn fps(&self) -> u32 {
        let s = self.0.lock().unwrap();
        match s.window {
            Some(w) if w.elapsed() < Duration::from_secs(2) => s.fps,
            _ => 0,
        }
    }

    pub fn encoding(&self) -> Option<&'static str> {
        self.0.lock().unwrap().encoding
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_once_per_interval_and_keeps_the_lowest_sample() {
        let s = Stats::default();
        assert!(s.ping_due());
        assert!(!s.ping_due());
        assert_eq!(s.latency(), None);
        s.update_received(Some("Tight"));
        let first = s.latency().unwrap();
        // An update with no ping outstanding adds no sample and keeps the encoding when none is given.
        s.update_received(None);
        assert_eq!(s.latency(), Some(first));
        assert_eq!(s.encoding(), Some("Tight"));
    }
}
