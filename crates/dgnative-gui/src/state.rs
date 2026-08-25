//! Pure UI state. No gpui, no BLE, so the interaction rules are unit-testable.

use dgnative::protocol::v3::{MAX_STRENGTH, builtin};

use crate::device::Found;

/// Which channel the strength controls act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    A,
    B,
    Both,
}

impl Target {
    pub fn uses_a(self) -> bool {
        matches!(self, Target::A | Target::Both)
    }

    pub fn uses_b(self) -> bool {
        matches!(self, Target::B | Target::Both)
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::A => "A",
            Target::B => "B",
            Target::Both => "A+B",
        }
    }
}

/// Connection state shown in the status bar.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Link {
    /// Not connected and not looking.
    Idle,
    Scanning,
    Connecting(String),
    Connected(String),
    Failed(String),
}

impl Link {
    pub fn is_connected(&self) -> bool {
        matches!(self, Link::Connected(_))
    }

    /// Scanning and connecting both block starting another one.
    pub fn is_busy(&self) -> bool {
        matches!(self, Link::Scanning | Link::Connecting(_))
    }
}

/// Step used by the soft limit buttons.
pub const LIMIT_STEP: u8 = 5;

/// Above this the UI shows a warning next to the soft limit.
pub const LIMIT_WARN: u8 = 50;

/// Everything the window draws, plus the rules for changing it.
///
/// `requested_*` is what the user asked for and what gets sent to the device;
/// `reported_*` is the last value the device confirmed in a B1 report. They
/// differ for a moment after every change, and permanently once the device
/// refuses to go higher.
pub struct UiState {
    pub link: Link,
    /// Running against the built-in simulator instead of real hardware.
    pub simulated: bool,
    /// Result of the last scan.
    pub devices: Vec<Found>,
    pub limit: u8,
    pub target: Target,
    pub wave: &'static str,
    pub requested_a: u8,
    pub requested_b: u8,
    pub reported_a: u8,
    pub reported_b: u8,
    pub battery: Option<u8>,
    pub error: Option<String>,
}

impl UiState {
    pub fn new(limit: u8) -> Self {
        UiState {
            link: Link::Idle,
            simulated: false,
            devices: Vec::new(),
            limit: limit.min(MAX_STRENGTH),
            target: Target::Both,
            wave: builtin::ALL[0].name,
            requested_a: 0,
            requested_b: 0,
            reported_a: 0,
            reported_b: 0,
            battery: None,
            error: None,
        }
    }

    /// Move the targeted channels by `delta`, clamped to `0..=limit`.
    pub fn adjust(&mut self, delta: i32) {
        if self.target.uses_a() {
            self.requested_a = step(self.requested_a, delta, self.limit);
        }
        if self.target.uses_b() {
            self.requested_b = step(self.requested_b, delta, self.limit);
        }
    }

    /// Zero both channels regardless of the current target.
    pub fn zero(&mut self) {
        self.requested_a = 0;
        self.requested_b = 0;
    }

    /// Change the soft limit; strengths above the new limit come down with it.
    pub fn set_limit(&mut self, limit: u8) {
        self.limit = limit.min(MAX_STRENGTH);
        self.requested_a = self.requested_a.min(self.limit);
        self.requested_b = self.requested_b.min(self.limit);
    }

    pub fn nudge_limit(&mut self, delta: i32) {
        self.set_limit(step(self.limit, delta, MAX_STRENGTH));
    }

    /// Fraction of the soft limit a channel is at, for the strength bars.
    pub fn fraction(&self, requested: u8) -> f32 {
        if self.limit == 0 {
            0.0
        } else {
            f32::from(requested) / f32::from(self.limit)
        }
    }

    pub fn limit_is_high(&self) -> bool {
        self.limit > LIMIT_WARN
    }
}

fn step(value: u8, delta: i32, max: u8) -> u8 {
    (i32::from(value) + delta).clamp(0, i32::from(max)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_is_clamped_to_the_soft_limit() {
        let mut state = UiState::new(20);
        state.adjust(30);
        assert_eq!(state.requested_a, 20);
        assert_eq!(state.requested_b, 20);
        state.adjust(-100);
        assert_eq!(state.requested_a, 0);
    }

    #[test]
    fn adjust_only_touches_the_targeted_channels() {
        let mut state = UiState::new(20);
        state.target = Target::A;
        state.adjust(5);
        assert_eq!((state.requested_a, state.requested_b), (5, 0));
        state.target = Target::B;
        state.adjust(3);
        assert_eq!((state.requested_a, state.requested_b), (5, 3));
    }

    #[test]
    fn lowering_the_limit_pulls_strength_down_with_it() {
        let mut state = UiState::new(40);
        state.adjust(40);
        state.set_limit(10);
        assert_eq!((state.requested_a, state.requested_b), (10, 10));
    }

    #[test]
    fn zero_ignores_the_target() {
        let mut state = UiState::new(20);
        state.adjust(10);
        state.target = Target::A;
        state.zero();
        assert_eq!((state.requested_a, state.requested_b), (0, 0));
    }

    #[test]
    fn limit_never_exceeds_the_protocol_maximum() {
        let mut state = UiState::new(255);
        assert_eq!(state.limit, MAX_STRENGTH);
        state.nudge_limit(100);
        assert_eq!(state.limit, MAX_STRENGTH);
        state.nudge_limit(-1000);
        assert_eq!(state.limit, 0);
    }

    #[test]
    fn fraction_is_safe_at_zero_limit() {
        let mut state = UiState::new(0);
        assert_eq!(state.fraction(0), 0.0);
        state.set_limit(50);
        state.adjust(25);
        assert_eq!(state.fraction(state.requested_a), 0.5);
    }

    #[test]
    fn defaults_start_silent() {
        let state = UiState::new(20);
        assert_eq!((state.requested_a, state.requested_b), (0, 0));
        assert_eq!(state.target, Target::Both);
    }
}
