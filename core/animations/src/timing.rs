//! The timing model of one animation: where on its timeline a moment falls,
//! which iteration that is, and how far through it, per CSS Animations and
//! the Web Animations timing model it is defined by.

use lumen_ir::animation::{AnimationDirectionIr, AnimationFillModeIr};

/// The timing an animation runs on, every value in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// One iteration's length.
    pub duration: f32,
    /// The wait before the first iteration; negative starts part-way in.
    pub delay: f32,
    /// How many iterations; `f32::INFINITY` for `infinite`.
    pub iterations: f32,
    /// Which way each iteration runs.
    pub direction: AnimationDirectionIr,
    /// Whether it applies before it starts and after it ends.
    pub fill: AnimationFillModeIr,
}

/// Where a moment falls on an animation's timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting out its delay.
    Before,
    /// Running an iteration.
    Active,
    /// Done.
    After,
}

/// What an animation shows at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// The phase the moment is in.
    pub phase: Phase,
    /// The iteration it is in, from 0.
    pub iteration: u64,
    /// How far through the keyframes, after the direction: `0` is the first
    /// keyframe, `1` the last. `None` when the animation applies nothing
    /// (a phase its fill mode does not cover).
    pub progress: Option<f32>,
    /// Whether this iteration runs the keyframes backwards.
    pub reversed: bool,
}

impl Timing {
    /// The length of every iteration together.
    pub fn active_duration(&self) -> f32 {
        if self.duration <= 0.0 || self.iterations <= 0.0 {
            0.0
        } else {
            self.duration * self.iterations
        }
    }

    /// When the animation ends, measured from its start.
    pub fn end_time(&self) -> f32 {
        (self.delay + self.active_duration()).max(0.0)
    }

    /// What it shows `local` seconds after it started (its delay included).
    pub fn sample(&self, local: f32) -> Sample {
        let active = self.active_duration();
        let backwards = matches!(
            self.fill,
            AnimationFillModeIr::Backwards | AnimationFillModeIr::Both
        );
        let forwards = matches!(
            self.fill,
            AnimationFillModeIr::Forwards | AnimationFillModeIr::Both
        );
        let at = local - self.delay;
        let (phase, overall) = if at < 0.0 {
            (Phase::Before, backwards.then_some(0.0))
        } else if at >= active {
            let end = if self.duration <= 0.0 {
                self.iterations.min(f32::MAX)
            } else {
                self.iterations
            };
            (Phase::After, forwards.then_some(end))
        } else {
            (Phase::Active, Some(at / self.duration))
        };
        let Some(overall) = overall else {
            return Sample {
                phase,
                iteration: 0,
                progress: None,
                reversed: false,
            };
        };
        // An animation that ends on a whole number of iterations shows the
        // end of its last one, not the start of one past it.
        let (iteration, within) =
            if phase == Phase::After && overall.fract() == 0.0 && overall > 0.0 {
                (overall as u64 - 1, 1.0)
            } else {
                (overall.floor() as u64, overall.fract())
            };
        let reversed = match self.direction {
            AnimationDirectionIr::Normal => false,
            AnimationDirectionIr::Reverse => true,
            AnimationDirectionIr::Alternate => iteration % 2 == 1,
            AnimationDirectionIr::AlternateReverse => iteration % 2 == 0,
        };
        Sample {
            phase,
            iteration,
            progress: Some(if reversed { 1.0 - within } else { within }),
            reversed,
        }
    }

    /// When the iteration a moment `local` seconds in falls in starts,
    /// measured from the animation's start.
    pub fn iteration_start(&self, iteration: u64) -> f32 {
        self.delay + iteration as f32 * self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(
        iterations: f32,
        direction: AnimationDirectionIr,
        fill: AnimationFillModeIr,
    ) -> Timing {
        Timing {
            duration: 1.0,
            delay: 0.5,
            iterations,
            direction,
            fill,
        }
    }

    #[test]
    fn the_delay_holds_nothing_unless_filled_backwards() {
        let t = timing(1.0, AnimationDirectionIr::Normal, AnimationFillModeIr::None);
        let s = t.sample(0.2);
        assert_eq!((s.phase, s.progress), (Phase::Before, None));
        let t = timing(
            1.0,
            AnimationDirectionIr::Reverse,
            AnimationFillModeIr::Backwards,
        );
        assert_eq!(
            t.sample(0.2).progress,
            Some(1.0),
            "reverse starts at the end"
        );
    }

    #[test]
    fn iterations_alternate_and_the_end_holds_when_filled_forwards() {
        let t = timing(
            3.0,
            AnimationDirectionIr::Alternate,
            AnimationFillModeIr::Forwards,
        );
        let s = t.sample(0.5 + 1.25);
        assert_eq!(s.iteration, 1);
        assert!(s.reversed);
        assert!((s.progress.unwrap() - 0.75).abs() < 1e-5);
        let end = t.sample(10.0);
        assert_eq!(end.phase, Phase::After);
        assert_eq!(end.iteration, 2);
        assert_eq!(end.progress, Some(1.0), "the third iteration runs forwards");
        let none = timing(
            3.0,
            AnimationDirectionIr::Alternate,
            AnimationFillModeIr::None,
        );
        assert_eq!(none.sample(10.0).progress, None);
    }

    #[test]
    fn a_fractional_count_ends_part_way_and_a_negative_delay_starts_part_way() {
        let t = Timing {
            duration: 2.0,
            delay: -1.0,
            iterations: 1.5,
            direction: AnimationDirectionIr::Normal,
            fill: AnimationFillModeIr::Forwards,
        };
        let s = t.sample(0.0);
        assert_eq!(s.phase, Phase::Active);
        assert!((s.progress.unwrap() - 0.5).abs() < 1e-5);
        let end = t.sample(5.0);
        assert_eq!(end.iteration, 1);
        assert!((end.progress.unwrap() - 0.5).abs() < 1e-5);
        assert_eq!(t.end_time(), 2.0);
    }

    #[test]
    fn infinite_never_ends() {
        let t = timing(
            f32::INFINITY,
            AnimationDirectionIr::Normal,
            AnimationFillModeIr::None,
        );
        assert_eq!(t.sample(1.0e6).phase, Phase::Active);
        assert_eq!(t.end_time(), f32::INFINITY);
    }
}
