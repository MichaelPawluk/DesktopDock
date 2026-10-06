//! Animation timing and curves. Pure functions of time, so they can be unit-tested.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

/// Moves `value` toward `target` at a rate that covers 0 → 1 in `up_ms` when rising or
/// `down_ms` when falling. Never overshoots. A duration of 0 jumps straight to the target.
pub fn step_toward(value: f32, target: f32, dt: f32, up_ms: u64, down_ms: u64) -> f32 {
    let (ms, rising) = if value < target { (up_ms, true) } else { (down_ms, false) };
    if ms == 0 {
        return target;
    }
    let step = dt * 1000.0 / ms as f32;
    if rising { (value + step).min(target) } else { (value - step).max(target) }
}

/// Fast start, gentle stop (cubic). Used in reverse it gives a gentle start, fast finish.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Launch bounce: two hops away from the screen edge, the second smaller.
/// `progress` runs from 0 to 1; outside that range there's no offset.
pub fn bounce_offset(progress: f32, amplitude: f32) -> f32 {
    if !(0.0..1.0).contains(&progress) {
        return 0.0;
    }
    amplitude * (progress * 2.0 * PI).sin().abs() * (1.0 - progress)
}

/// Launch failed: three quick side-to-side shakes that settle. `progress` runs from 0 to 1.
pub fn shake_offset(progress: f32, amplitude: f32) -> f32 {
    if !(0.0..1.0).contains(&progress) {
        return 0.0;
    }
    amplitude * (progress * 6.0 * PI).sin() * (1.0 - progress)
}

/// Fires once a condition has held without interruption for a set time, such as the
/// pointer resting at the screen edge for the popup delay.
#[derive(Debug, Default)]
pub struct Dwell {
    since: Option<Instant>,
}

impl Dwell {
    pub fn update(&mut self, holding: bool, now: Instant, delay: Duration) -> bool {
        if !holding {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.duration_since(since) >= delay
    }

    pub fn reset(&mut self) {
        self.since = None;
    }
}

/// Ignores a second click on the same item shortly after it launched, so a habitual
/// double-click opens one copy. The window counts from the last launch that went through.
#[derive(Debug, Default)]
pub struct ClickGuard {
    last: Option<(usize, Instant)>,
}

impl ClickGuard {
    pub const WINDOW: Duration = Duration::from_millis(500);

    /// True if this click should launch the item.
    pub fn allow(&mut self, item: usize, now: Instant) -> bool {
        if let Some((last_item, at)) = self.last {
            if last_item == item && now.duration_since(at) < Self::WINDOW {
                return false;
            }
        }
        self.last = Some((item, now));
        true
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_click_launches_once() {
        let t = Instant::now();
        let mut guard = ClickGuard::default();
        assert!(guard.allow(3, t));
        assert!(!guard.allow(3, t + Duration::from_millis(250)), "second click of a double-click");
    }

    #[test]
    fn clicking_again_later_still_launches() {
        let t = Instant::now();
        let mut guard = ClickGuard::default();
        assert!(guard.allow(3, t));
        assert!(guard.allow(3, t + Duration::from_millis(500)));
    }

    #[test]
    fn a_different_icon_is_never_blocked() {
        let t = Instant::now();
        let mut guard = ClickGuard::default();
        assert!(guard.allow(3, t));
        assert!(guard.allow(4, t + Duration::from_millis(100)));
    }

    #[test]
    fn ignored_clicks_dont_extend_the_wait() {
        // Triple-click: 0 ms launches, 250 ms ignored, 600 ms launches (600 ms after the first).
        let t = Instant::now();
        let mut guard = ClickGuard::default();
        assert!(guard.allow(3, t));
        assert!(!guard.allow(3, t + Duration::from_millis(250)));
        assert!(guard.allow(3, t + Duration::from_millis(600)));
    }

    #[test]
    fn step_covers_the_range_in_the_given_time() {
        // 250 ms slide: after 125 ms we're halfway.
        assert!((step_toward(0.0, 1.0, 0.125, 250, 999) - 0.5).abs() < 1e-6);
        // Falling uses the other duration.
        assert!((step_toward(1.0, 0.0, 0.1, 999, 200) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn step_never_overshoots() {
        assert_eq!(step_toward(0.9, 1.0, 1.0, 250, 250), 1.0);
        assert_eq!(step_toward(0.1, 0.0, 1.0, 250, 250), 0.0);
    }

    #[test]
    fn zero_duration_jumps() {
        assert_eq!(step_toward(0.0, 1.0, 0.001, 0, 250), 1.0);
        assert_eq!(step_toward(1.0, 0.0, 0.001, 250, 0), 0.0);
    }

    #[test]
    fn at_target_stays_put() {
        assert_eq!(step_toward(1.0, 1.0, 0.016, 250, 250), 1.0);
    }

    #[test]
    fn ease_out_runs_from_0_to_1_without_going_backwards() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease_out(i as f32 / 100.0);
            assert!(v >= last);
            last = v;
        }
        // Clamped outside 0..1.
        assert_eq!(ease_out(-1.0), 0.0);
        assert_eq!(ease_out(2.0), 1.0);
    }

    #[test]
    fn bounce_is_two_hops_that_settle() {
        let amp = 12.0;
        assert_eq!(bounce_offset(0.0, amp), 0.0);
        assert_eq!(bounce_offset(1.0, amp), 0.0);
        assert!(bounce_offset(0.5, amp).abs() < 1e-4, "lands between hops");
        let first = bounce_offset(0.25, amp);
        let second = bounce_offset(0.75, amp);
        assert!(first > 0.0 && second > 0.0);
        assert!(second < first, "second hop is smaller");
        for i in 0..100 {
            assert!(bounce_offset(i as f32 / 100.0, amp) <= amp);
        }
    }

    #[test]
    fn shake_goes_both_ways_and_settles() {
        let amp = 6.0;
        assert_eq!(shake_offset(0.0, amp), 0.0);
        assert_eq!(shake_offset(1.0, amp), 0.0);
        let samples: Vec<f32> = (0..100).map(|i| shake_offset(i as f32 / 100.0, amp)).collect();
        assert!(samples.iter().any(|&x| x > 1.0) && samples.iter().any(|&x| x < -1.0), "left and right");
        assert!(samples.iter().all(|x| x.abs() <= amp));
        assert!(shake_offset(0.92, amp).abs() < shake_offset(0.08, amp).abs(), "dies down");
    }

    #[test]
    fn dwell_fires_only_after_holding_long_enough() {
        let start = Instant::now();
        let delay = Duration::from_millis(350);
        let mut dwell = Dwell::default();
        assert!(!dwell.update(true, start, delay));
        assert!(!dwell.update(true, start + Duration::from_millis(300), delay));
        assert!(dwell.update(true, start + Duration::from_millis(350), delay));
    }

    #[test]
    fn dwell_restarts_when_interrupted() {
        let start = Instant::now();
        let delay = Duration::from_millis(350);
        let mut dwell = Dwell::default();
        dwell.update(true, start, delay);
        // A quick flick away (e.g. towards a browser tab) restarts the wait.
        assert!(!dwell.update(false, start + Duration::from_millis(200), delay));
        assert!(!dwell.update(true, start + Duration::from_millis(400), delay));
        assert!(dwell.update(true, start + Duration::from_millis(750), delay));
    }
}
