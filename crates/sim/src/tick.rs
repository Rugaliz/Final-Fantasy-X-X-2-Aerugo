//! Fixed-timestep driver: turns real elapsed time into a whole number of simulation ticks plus the
//! fraction of the next tick that has elapsed (used for interpolation).

use std::time::Duration;

/// Tick rate of the original games' logic.
pub const GAME_TICK_HZ: u32 = 30;

/// Result of advancing the clock for one displayed frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Steps {
    /// Simulation ticks to run now (0..=max_catch_up).
    pub ticks: u32,
    /// Fraction of a tick elapsed after those ticks, in `[0, 1)`.
    pub alpha: f32,
    /// True if time was discarded because the simulation fell too far behind.
    pub dropped_time: bool,
}

#[derive(Debug, Clone)]
pub struct FixedTimestep {
    period: Duration,
    accumulator: Duration,
    max_catch_up: u32,
}

impl FixedTimestep {
    /// `rate_hz` ticks per second; at most `max_catch_up` ticks are run per frame. When the backlog is
    /// larger, the excess is dropped, so a slow machine slows the game down instead of skipping logic.
    pub fn new(rate_hz: u32, max_catch_up: u32) -> Self {
        assert!(rate_hz > 0 && max_catch_up > 0);
        Self {
            period: Duration::from_nanos(1_000_000_000 / rate_hz as u64),
            accumulator: Duration::ZERO,
            max_catch_up,
        }
    }

    pub fn period(&self) -> Duration {
        self.period
    }

    pub fn advance(&mut self, elapsed: Duration) -> Steps {
        self.accumulator += elapsed;
        let mut ticks = 0;
        while self.accumulator >= self.period && ticks < self.max_catch_up {
            self.accumulator -= self.period;
            ticks += 1;
        }
        let dropped_time = self.accumulator >= self.period;
        if dropped_time {
            // keep the fractional part so interpolation stays continuous
            self.accumulator =
                Duration::from_nanos((self.accumulator.as_nanos() % self.period.as_nanos()) as u64);
        }
        let alpha = (self.accumulator.as_secs_f64() / self.period.as_secs_f64()) as f32;
        Steps { ticks, alpha: alpha.clamp(0.0, 0.999_999), dropped_time }
    }

    /// Forget accumulated time (after a pause or a scene load).
    pub fn reset(&mut self) {
        self.accumulator = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_ticks_and_fraction() {
        let mut t = FixedTimestep::new(30, 4);
        let period = t.period();
        let s = t.advance(period / 2);
        assert_eq!(s.ticks, 0);
        assert!((s.alpha - 0.5).abs() < 1e-3);
        let s = t.advance(period);
        assert_eq!(s.ticks, 1);
        assert!((s.alpha - 0.5).abs() < 1e-3);
    }

    #[test]
    fn high_display_rate_runs_exactly_30_ticks_per_second() {
        let mut t = FixedTimestep::new(30, 4);
        let frame = Duration::from_nanos(1_000_000_000 / 240);
        let ticks: u32 = (0..240).map(|_| t.advance(frame).ticks).sum();
        assert!((29..=30).contains(&ticks), "{ticks}");
    }

    #[test]
    fn long_stall_is_capped() {
        let mut t = FixedTimestep::new(30, 4);
        let s = t.advance(Duration::from_secs(2));
        assert_eq!(s.ticks, 4);
        assert!(s.dropped_time);
        assert!(s.alpha < 1.0);
        assert_eq!(t.advance(Duration::ZERO).ticks, 0);
    }
}
