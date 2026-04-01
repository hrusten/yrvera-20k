//! Authentic power bar animation state for the sidebar.
//!
//! Implements the original RA2 `PowerClass` (Power.CPP) animation system.
//! Three segment counters (surplus/output/drain) slide toward target values
//! one segment at a time, with a flash phase on power changes.
//!
//! Draw order top-to-bottom: empty → blink → surplus (green, frame 1)
//! → output (yellow, frame 2) → drain (red, frame 3).
//!
//! ## Dependency rules
//! - Part of sidebar/ — pure data + logic, no rendering or sim dependencies.

/// Segment height in pixels (each powerp.shp frame is 3px tall in the original).
const SEGMENT_HEIGHT_PX: i32 = 3;

/// Number of flashes when power values change (original: 10).
const FLASH_COUNT: i32 = 10;

/// Ticks between animation steps.
/// Original game: 3 ticks at ~15Hz = ~200ms per step.
/// Our engine: 9 ticks at 45Hz = ~200ms per step (same wall-clock speed).
const TICKS_PER_STEP: i32 = 9;

/// Scale factor for the asymptotic bar fill curve (original: 400.0 at 0x7ED8C8).
/// When total theoretical power equals this value, the bar is exactly half full.
const FILL_SCALE: f64 = 400.0;

/// Cap on the "output portion" before surplus kicks in (original: 100.0 at 0x7E2AC0).
/// When surplus (output - drain) exceeds this, the excess becomes the surplus band.
const OUTPUT_CAP: f64 = 100.0;

/// Persistent animation state for the sidebar power bar.
///
/// Tracks three segment counts that animate toward target values one segment at
/// a time. Segment names match the original binary's verified semantics:
/// - surplus = excess power (green, frame 1, top of filled area)
/// - output = production matching drain (yellow, frame 2, middle)
/// - drain = power consumption (red, frame 3, bottom of bar)
#[derive(Debug, Clone)]
pub struct PowerBarAnimState {
    /// Current animated surplus segment count (green, top of filled area).
    pub surplus_segments: i32,
    /// Current animated output segment count (yellow, middle).
    pub output_segments: i32,
    /// Current animated drain segment count (red, bottom).
    pub drain_segments: i32,

    /// Target surplus segments (computed from power values).
    target_surplus: i32,
    /// Target output segments.
    target_output: i32,
    /// Target drain segments.
    target_drain: i32,

    /// Maximum segments that fit in the bar (bar_height / 3).
    max_segments: i32,

    /// Last-seen power output — used to detect changes.
    cached_output: i32,
    /// Last-seen power drain — used to detect changes.
    cached_drain: i32,
    /// Last-seen theoretical total — used to detect changes.
    cached_theoretical: i32,

    /// Remaining flash blinks (starts at FLASH_COUNT on power change).
    flashes_remaining: i32,
    /// Tick counter for flash timing (counts down from TICKS_PER_STEP).
    flash_tick_counter: i32,

    /// Tick counter for segment animation (counts down from TICKS_PER_STEP).
    anim_tick_counter: i32,
    /// True while current segments differ from targets.
    animating: bool,

    /// Whether initial values have been set (first update jumps instantly).
    initialized: bool,
}

impl Default for PowerBarAnimState {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerBarAnimState {
    pub fn new() -> Self {
        Self {
            surplus_segments: 0,
            output_segments: 0,
            drain_segments: 0,
            target_surplus: 0,
            target_output: 0,
            target_drain: 0,
            max_segments: 0,
            cached_output: -1,
            cached_drain: -1,
            cached_theoretical: -1,
            flashes_remaining: 0,
            flash_tick_counter: 0,
            anim_tick_counter: 0,
            animating: false,
            initialized: false,
        }
    }

    /// Update the bar capacity from the available pixel height.
    pub fn set_max_segments(&mut self, bar_height_px: i32) {
        let new_max = bar_height_px / SEGMENT_HEIGHT_PX;
        if new_max != self.max_segments {
            self.max_segments = new_max;
            if self.initialized {
                self.compute_targets(
                    self.cached_output,
                    self.cached_drain,
                    self.cached_theoretical,
                );
                self.clamp_segments();
            }
        }
    }

    /// Feed current power values. Detects changes and starts flash/animation.
    ///
    /// - `power_output`: health-scaled operational output (for segment distribution)
    /// - `power_drain`: full-rated operational drain (for segment distribution)
    /// - `theoretical_total`: sum of |Power=| from TypeClass for ALL buildings (for fill curve)
    pub fn update(&mut self, power_output: i32, power_drain: i32, theoretical_total: i32) {
        if !self.initialized {
            self.initialized = true;
            self.cached_output = power_output;
            self.cached_drain = power_drain;
            self.cached_theoretical = theoretical_total;
            self.compute_targets(power_output, power_drain, theoretical_total);
            self.surplus_segments = self.target_surplus;
            self.output_segments = self.target_output;
            self.drain_segments = self.target_drain;
            return;
        }

        if power_output != self.cached_output
            || power_drain != self.cached_drain
            || theoretical_total != self.cached_theoretical
        {
            self.cached_output = power_output;
            self.cached_drain = power_drain;
            self.cached_theoretical = theoretical_total;
            self.compute_targets(power_output, power_drain, theoretical_total);

            // Start flash phase.
            self.flashes_remaining = FLASH_COUNT;
            self.flash_tick_counter = TICKS_PER_STEP;

            // Start animation phase.
            self.animating = true;
            self.anim_tick_counter = TICKS_PER_STEP;
        }
    }

    /// Advance one simulation tick. Handles flash countdown and segment animation.
    pub fn tick(&mut self) {
        if !self.initialized {
            return;
        }

        // Flash phase: count down flashes.
        if self.flashes_remaining > 0 {
            self.flash_tick_counter -= 1;
            if self.flash_tick_counter <= 0 {
                self.flashes_remaining -= 1;
                self.flash_tick_counter = TICKS_PER_STEP;
            }
        }

        // Segment animation: move one segment toward target per step.
        if self.animating {
            self.anim_tick_counter -= 1;
            if self.anim_tick_counter <= 0 {
                self.anim_tick_counter = TICKS_PER_STEP;
                self.step_one_segment();
            }
        }
    }

    /// Whether the bar is currently in the flash/blink phase.
    /// Even counter values = draw blink (matching binary's `counter & 0x80000001`).
    /// Counter starts at 10 (even) so the first frame IS blinking.
    pub fn is_flashing(&self) -> bool {
        self.flashes_remaining > 0 && (self.flashes_remaining % 2 == 0)
    }

    /// Returns `(empty, surplus, output, drain)` segment counts for drawing.
    /// Draw order top-to-bottom: empty → surplus(green) → output(yellow) → drain(red).
    pub fn segment_counts(&self) -> (i32, i32, i32, i32) {
        let filled = self.surplus_segments + self.output_segments + self.drain_segments;
        let empty = (self.max_segments - filled).max(0);
        (
            empty,
            self.surplus_segments,
            self.output_segments,
            self.drain_segments,
        )
    }

    /// The maximum number of segments the bar can display.
    pub fn max_segments(&self) -> i32 {
        self.max_segments
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    /// Compute target segment distribution from power values.
    ///
    /// Phase 1: Total filled segments via asymptotic curve (original Calc_Segments).
    ///   `filled = max_segs - ftol(max_segs * 400 / (theoretical_total + 400))`
    ///   Bar is half-full when theoretical total power = 400.
    ///
    /// Phase 2: Split filled into drain/output/surplus (original Calc_Power_Distribution).
    ///   surplus_raw = output - drain. Output portion capped at 100 before surplus
    ///   kicks in. Three zones drawn proportionally within the filled area.
    fn compute_targets(&mut self, power_output: i32, power_drain: i32, theoretical_total: i32) {
        if self.max_segments <= 0 {
            self.target_surplus = 0;
            self.target_output = 0;
            self.target_drain = 0;
            return;
        }

        // Phase 1: Filled segments (asymptotic curve from original Calc_Segments).
        let total = theoretical_total.max(0) as f64;
        let empty_ratio = FILL_SCALE / (total + FILL_SCALE);
        let empty = (self.max_segments as f64 * empty_ratio) as i32;
        let empty = empty.clamp(0, self.max_segments - 1);
        let filled = self.max_segments - empty;

        // Phase 2: Drain/output/surplus split (original Calc_Power_Distribution).
        let drain = power_drain.max(0) as f64;
        let output = power_output.max(0) as f64;
        let surplus_raw = output - drain;

        let (output_portion, surplus_portion) = if surplus_raw < 0.0 {
            (0.0, 0.0)
        } else if surplus_raw < OUTPUT_CAP {
            (surplus_raw, 0.0)
        } else {
            (OUTPUT_CAP, surplus_raw - OUTPUT_CAP)
        };

        let sum = drain + output_portion + surplus_portion;
        let (drain_frac, output_frac, surplus_frac) = if sum > 0.0 {
            (drain / sum, output_portion / sum, surplus_portion / sum)
        } else {
            (1.0, 0.0, 0.0)
        };

        let filled_f = filled as f64;
        self.target_drain = (filled_f * drain_frac) as i32;
        self.target_output = (filled_f * output_frac) as i32;
        self.target_surplus = (filled_f * surplus_frac) as i32;

        // Rounding residual goes to drain (matches original + 0.01 epsilon behavior).
        let residual = filled - self.target_drain - self.target_output - self.target_surplus;
        self.target_drain += residual;
    }

    /// Move one segment toward the target.
    /// Priority: surplus first, then drain, then output (matches original).
    fn step_one_segment(&mut self) {
        if self.surplus_segments != self.target_surplus {
            if self.surplus_segments < self.target_surplus {
                self.surplus_segments += 1;
            } else {
                self.surplus_segments -= 1;
            }
        } else if self.drain_segments != self.target_drain {
            if self.drain_segments < self.target_drain {
                self.drain_segments += 1;
            } else {
                self.drain_segments -= 1;
            }
        } else if self.output_segments != self.target_output {
            if self.output_segments < self.target_output {
                self.output_segments += 1;
            } else {
                self.output_segments -= 1;
            }
        } else {
            self.animating = false;
        }

        self.clamp_segments();
    }

    /// Ensure segment counts stay within valid bounds.
    fn clamp_segments(&mut self) {
        self.surplus_segments = self.surplus_segments.clamp(0, self.max_segments);
        self.output_segments = self.output_segments.clamp(0, self.max_segments);
        self.drain_segments = self.drain_segments.clamp(0, self.max_segments);

        let total = self.surplus_segments + self.output_segments + self.drain_segments;
        if total > self.max_segments {
            let excess = total - self.max_segments;
            let drain_reduce = excess.min(self.drain_segments);
            self.drain_segments -= drain_reduce;
            let remaining = excess - drain_reduce;
            self.output_segments = (self.output_segments - remaining).max(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_jump_to_target() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150); // 50 segments
        anim.update(200, 100, 300);

        assert_eq!(anim.surplus_segments, anim.target_surplus);
        assert_eq!(anim.output_segments, anim.target_output);
        assert_eq!(anim.drain_segments, anim.target_drain);
        assert_eq!(anim.flashes_remaining, 0, "no flash on initial set");
    }

    #[test]
    fn test_asymptotic_fill_curve() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(300); // 100 segments

        // At theoretical=400, bar should be half full (50 filled).
        anim.update(200, 200, 400);
        let (empty, surplus, output, drain) = anim.segment_counts();
        let filled = surplus + output + drain;
        assert_eq!(filled, 50, "bar should be half full at theoretical=400");
        assert_eq!(empty, 50);

        // At theoretical=0, bar should have 1 filled segment (minimum).
        let mut anim2 = PowerBarAnimState::new();
        anim2.set_max_segments(300);
        anim2.update(0, 0, 0);
        let (empty2, surplus2, output2, drain2) = anim2.segment_counts();
        let filled2 = surplus2 + output2 + drain2;
        assert_eq!(filled2, 1, "bar should have at least 1 filled segment");
        assert_eq!(empty2, 99);
    }

    #[test]
    fn test_surplus_distribution() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(300); // 100 segments
        // Output 300, drain 100 → surplus = 200. output_portion = 100, surplus_portion = 100.
        // Sum = 100 + 100 + 100 = 300. Each fraction = 1/3.
        anim.update(300, 100, 400);
        let (_, surplus, output, drain) = anim.segment_counts();
        // With theoretical=400, filled=50.
        // Each should be roughly 50/3 ≈ 16-17.
        assert!(drain > 0, "drain should be nonzero");
        assert!(output > 0, "output should be nonzero");
        assert!(surplus > 0, "surplus should be nonzero");
        assert_eq!(
            surplus + output + drain,
            50,
            "segments should sum to filled"
        );
    }

    #[test]
    fn test_deficit_all_drain() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(300); // 100 segments
        // Output=0, drain=200 → surplus_raw = -200. output_portion=0, surplus_portion=0.
        // All filled segments should be drain.
        anim.update(0, 200, 200);
        let (_, surplus, output, drain) = anim.segment_counts();
        assert_eq!(surplus, 0);
        assert_eq!(output, 0);
        assert!(drain > 0, "all filled should be drain");
    }

    #[test]
    fn test_zero_power_minimal_bar() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150); // 50 segments
        anim.update(0, 0, 0);

        let (empty, surplus, output, drain) = anim.segment_counts();
        let filled = surplus + output + drain;
        assert_eq!(filled, 1, "minimum 1 filled segment");
        // With 0 power, sum=0 → default all-drain fraction.
        assert_eq!(drain, 1);
        assert_eq!(empty, 49);
    }

    #[test]
    fn test_animation_starts_on_change() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150);
        anim.update(200, 100, 300); // initial

        anim.update(200, 150, 350); // change
        assert_eq!(anim.flashes_remaining, FLASH_COUNT);
        assert!(anim.animating);
    }

    #[test]
    fn test_no_animation_on_same_values() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150);
        anim.update(200, 100, 300);

        anim.update(200, 100, 300); // same values
        assert_eq!(anim.flashes_remaining, 0);
    }

    #[test]
    fn test_segment_animation_converges() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150); // 50 segments
        anim.update(200, 100, 300); // initial

        anim.update(200, 0, 200); // change

        for _ in 0..(50 * TICKS_PER_STEP + 100) {
            anim.tick();
        }

        assert_eq!(anim.surplus_segments, anim.target_surplus);
        assert_eq!(anim.output_segments, anim.target_output);
        assert_eq!(anim.drain_segments, anim.target_drain);
        assert!(!anim.animating);
    }

    #[test]
    fn test_flash_even_blink() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150);
        anim.update(200, 100, 300); // initial
        anim.update(200, 50, 250); // change → starts flash at 10

        // Counter=10 (even) → is_flashing() should be true.
        assert!(anim.is_flashing(), "even counter should blink");

        // Tick through one flash step → counter becomes 9 (odd).
        for _ in 0..TICKS_PER_STEP {
            anim.tick();
        }
        assert!(!anim.is_flashing(), "odd counter should not blink");

        // Another step → counter becomes 8 (even).
        for _ in 0..TICKS_PER_STEP {
            anim.tick();
        }
        assert!(anim.is_flashing(), "even counter should blink again");
    }

    #[test]
    fn test_flash_timer_decrements() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150);
        anim.update(200, 100, 300);
        anim.update(200, 50, 250);

        assert_eq!(anim.flashes_remaining, FLASH_COUNT);

        for _ in 0..(FLASH_COUNT * TICKS_PER_STEP) {
            anim.tick();
        }

        assert_eq!(anim.flashes_remaining, 0);
    }

    #[test]
    fn test_segment_counts_sum_correctly() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(150); // 50 segments
        anim.update(200, 100, 300);

        let (empty, surplus, output, drain) = anim.segment_counts();
        assert_eq!(empty + surplus + output + drain, 50);
    }

    #[test]
    fn test_equal_power_drain() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(300); // 100 segments
        anim.update(100, 100, 200);

        // surplus_raw = 0. output_portion=0, surplus_portion=0.
        // All filled = drain.
        let (_, surplus, output, drain) = anim.segment_counts();
        assert_eq!(surplus, 0, "no surplus when output == drain");
        assert_eq!(output, 0, "no output band when surplus_raw == 0");
        assert!(drain > 0, "all filled should be drain");
    }

    #[test]
    fn test_small_surplus_goes_to_output_band() {
        let mut anim = PowerBarAnimState::new();
        anim.set_max_segments(300); // 100 segments
        // Output=150, drain=100 → surplus_raw=50 < 100 cap.
        // output_portion=50, surplus_portion=0.
        anim.update(150, 100, 250);

        let (_, surplus, output, drain) = anim.segment_counts();
        assert_eq!(surplus, 0, "surplus_portion is 0 when surplus < 100");
        assert!(output > 0, "output band should exist for small surplus");
        assert!(drain > 0, "drain should be nonzero");
    }
}
