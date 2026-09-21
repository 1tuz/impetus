//! Paced reveal for assistant stream chunks (arrival ≠ paint).
//!
//! Adapted from JCode `StreamBuffer` idea: provider bursts accumulate in a
//! backlog; a time-paced proportional controller drips characters into the
//! timeline so the TUI does not stair-step. Text-only — no reasoning regions,
//! no jitter metrics (YAGNI for Phase 7).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Steady-state reveal rate (chars/sec) when the backlog is empty.
const BASE_REVEAL_CPS: f32 = 180.0;

/// Additional reveal rate per buffered character.
const REVEAL_BACKLOG_GAIN: f32 = 3.0;

/// Hard ceiling for paced output (chars/sec).
const MAX_REVEAL_CPS: f32 = 960.0;

/// Maximum elapsed time credited to a single reveal step.
const MAX_REVEAL_STEP: Duration = Duration::from_millis(50);

/// Buffer that accumulates streaming text and reveals it at a smooth rate.
#[derive(Debug)]
pub struct StreamBuffer {
    queue: VecDeque<String>,
    backlog_chars: usize,
    last_reveal: Instant,
    carry: f32,
    ceiling_carry: f32,
}

impl Default for StreamBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamBuffer {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            backlog_chars: 0,
            last_reveal: Instant::now(),
            carry: 0.0,
            ceiling_carry: 0.0,
        }
    }

    /// Push answer text; return any paced fragment ready to paint now.
    pub fn push_text(&mut self, text: &str) -> String {
        if text.is_empty() {
            return self.reveal_now(Instant::now());
        }
        self.push_chunk(text);
        self.reveal_now(Instant::now())
    }

    /// Drain the entire backlog (final / cancel / disconnect).
    pub fn flush(&mut self) -> String {
        self.carry = 0.0;
        self.ceiling_carry = 0.0;
        self.last_reveal = Instant::now();
        let text = self.drain_chars(self.backlog_chars);
        debug_assert!(self.queue.is_empty());
        self.backlog_chars = 0;
        text
    }

    /// Reveal one paced frame; call from the UI tick even when no delta arrived.
    pub fn flush_smooth_frame(&mut self) -> String {
        self.reveal_now(Instant::now())
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Drop backlog without returning content (session switch / clear viewport).
    pub fn clear(&mut self) {
        self.queue.clear();
        self.backlog_chars = 0;
        self.carry = 0.0;
        self.ceiling_carry = 0.0;
        self.last_reveal = Instant::now();
    }

    fn push_chunk(&mut self, text: &str) {
        self.backlog_chars += text.chars().count();
        if let Some(last) = self.queue.back_mut() {
            last.push_str(text);
            return;
        }
        self.queue.push_back(text.to_owned());
    }

    fn reveal_now(&mut self, now: Instant) -> String {
        if self.backlog_chars == 0 {
            self.carry = 0.0;
            self.ceiling_carry = 0.0;
            self.last_reveal = now;
            return String::new();
        }

        let dt = now
            .saturating_duration_since(self.last_reveal)
            .min(MAX_REVEAL_STEP)
            .as_secs_f32();
        self.last_reveal = now;

        let cps = BASE_REVEAL_CPS + self.backlog_chars as f32 * REVEAL_BACKLOG_GAIN;
        self.carry += dt * cps;
        self.ceiling_carry += dt * MAX_REVEAL_CPS;

        let controller_budget = self.carry.floor() as usize;
        let ceiling_budget = self.ceiling_carry.floor() as usize;
        let mut reveal = controller_budget.min(ceiling_budget);
        if reveal == 0 {
            return String::new();
        }

        reveal = reveal.min(self.backlog_chars);
        self.carry -= reveal as f32;
        self.ceiling_carry -= reveal as f32;
        self.drain_chars(reveal)
    }

    fn drain_chars(&mut self, mut char_count: usize) -> String {
        let mut out = String::new();
        while char_count > 0 {
            let Some(text) = self.queue.front_mut() else {
                break;
            };
            let available = text.chars().count();
            let take = char_count.min(available);
            if take == available {
                let chunk = self.queue.pop_front().expect("front");
                out.push_str(&chunk);
            } else {
                let end = text
                    .char_indices()
                    .nth(take)
                    .map(|(idx, _)| idx)
                    .unwrap_or(text.len());
                out.push_str(&text[..end]);
                text.replace_range(..end, "");
            }
            char_count -= take;
            self.backlog_chars = self.backlog_chars.saturating_sub(take);
        }
        out
    }

    /// Test/helper: set last reveal clock without waiting on wall time.
    #[cfg(test)]
    fn set_last_reveal(&mut self, at: Instant) {
        self.last_reveal = at;
    }

    /// Test/helper: reveal at an explicit instant.
    #[cfg(test)]
    fn reveal_at(&mut self, now: Instant) -> String {
        self.reveal_now(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain_frames(buf: &mut StreamBuffer, start: Instant, frame: Duration) -> Vec<usize> {
        let mut sizes = Vec::new();
        let mut t = start;
        let mut guard = 0;
        while !buf.is_empty() {
            t += frame;
            let text = buf.reveal_at(t);
            let chars = text.chars().count();
            if chars > 0 {
                sizes.push(chars);
            }
            guard += 1;
            assert!(guard < 100_000, "drain did not converge");
        }
        sizes
    }

    #[test]
    fn flush_drains_everything() {
        let mut buf = StreamBuffer::new();
        buf.push_chunk("remaining content");
        let text = buf.flush();
        assert_eq!(text, "remaining content");
        assert!(buf.is_empty());
    }

    #[test]
    fn empty_push_reveals_nothing() {
        let mut buf = StreamBuffer::new();
        assert!(buf.push_text("").is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn paced_reveal_spreads_a_burst_over_multiple_frames() {
        let start = Instant::now();
        let mut buf = StreamBuffer::new();
        buf.set_last_reveal(start);
        buf.push_chunk(&"a".repeat(40));

        let sizes = drain_frames(&mut buf, start, Duration::from_millis(16));
        let total: usize = sizes.iter().sum();
        assert_eq!(total, 40);
        assert!(
            sizes.len() >= 3,
            "a 40-char burst should reveal across multiple frames, got {sizes:?}"
        );
        assert!(
            sizes.iter().all(|&n| n < 40),
            "no frame should reveal the entire burst, got {sizes:?}"
        );
    }

    #[test]
    fn large_single_burst_is_bounded_by_wall_clock_reveal_rate() {
        let start = Instant::now();
        let mut buf = StreamBuffer::new();
        buf.set_last_reveal(start);
        buf.push_chunk(&"a".repeat(3_356));

        let sizes = drain_frames(&mut buf, start, Duration::from_millis(50));
        assert_eq!(sizes.iter().sum::<usize>(), 3_356);
        assert!(
            sizes.iter().all(|&n| n <= 48),
            "a 50ms paced frame must reveal at most 48 chars: {sizes:?}"
        );
        assert!(
            sizes.len() >= 70,
            "the burst should drain smoothly over several seconds: {} frames",
            sizes.len()
        );
    }

    #[test]
    fn frequent_push_calls_cannot_bypass_the_wall_clock_ceiling() {
        let start = Instant::now();
        let mut buf = StreamBuffer::new();
        buf.set_last_reveal(start);
        buf.push_chunk(&"c".repeat(1_000));

        let first_at = start + Duration::from_millis(50);
        let mut revealed = buf.reveal_at(first_at).chars().count();
        for _ in 0..100 {
            revealed += buf.reveal_at(first_at).chars().count();
        }
        assert_eq!(revealed, 48);

        let second = buf
            .reveal_at(first_at + Duration::from_millis(50))
            .chars()
            .count();
        assert_eq!(second, 48);
    }

    #[test]
    fn clear_drops_backlog_without_return() {
        let mut buf = StreamBuffer::new();
        buf.push_chunk("gone");
        buf.clear();
        assert!(buf.is_empty());
    }
}
