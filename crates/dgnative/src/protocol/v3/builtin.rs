//! Built-in waveform data in V3 format.
//!
//! [`BREATHING`] and [`TIDE`] come from the official doc `coyote/v3/example.md`
//! (the official App's built-in waveforms); the rest are custom to this crate,
//! distinguished by [`Builtin::official`].
//!
//! All of them are **single-channel** data, meant to be combined into a full
//! A/B channel B0 command. Each frame represents 100ms (i.e. the 4 groups of
//! 25ms data for one channel in a B0 command, all 4 identical within a frame).

use super::Pulse;

/// A single built-in waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    /// Waveform name (the id used on the command line).
    pub name: &'static str,
    /// Display label.
    pub label: &'static str,
    /// Whether this is original data from the official App. Custom waveforms
    /// are `false`.
    pub official: bool,
    /// `(waveform frequency, waveform strength)` per frame, one frame is 100ms.
    pub frames: &'static [(u8, u8)],
}

impl Builtin {
    /// Take frame `index` (wrapping around the frame count automatically) and
    /// expand it into the 4 groups of waveform data a B0 command needs.
    pub fn pulses_at(&self, index: usize) -> [Pulse; 4] {
        let (frequency, intensity) = self.frames[index % self.frames.len()];
        [Pulse {
            frequency,
            intensity,
        }; 4]
    }

    /// Duration of one full cycle, in milliseconds.
    pub fn cycle_ms(&self) -> u64 {
        self.frames.len() as u64 * super::B0_INTERVAL_MS
    }
}

/// Breathing.
pub const BREATHING: Builtin = Builtin {
    name: "breathing",
    label: "Breathing",
    official: true,
    frames: &[
        (10, 0),
        (10, 20),
        (10, 40),
        (10, 60),
        (10, 80),
        (10, 100),
        (10, 100),
        (10, 100),
        (10, 0),
        (10, 0),
        (10, 0),
        (10, 0),
    ],
};

/// Tide.
pub const TIDE: Builtin = Builtin {
    name: "tide",
    label: "Tide",
    official: true,
    frames: &[
        (10, 0),
        (11, 16),
        (13, 33),
        (14, 50),
        (16, 66),
        (18, 83),
        (19, 100),
        (21, 92),
        (22, 84),
        (24, 76),
        (26, 68),
        (26, 0),
        (27, 16),
        (29, 33),
        (30, 50),
        (32, 66),
        (34, 83),
        (35, 100),
        (37, 92),
        (38, 84),
        (40, 76),
        (42, 68),
        (10, 0),
    ],
};

/// Constant full power.
///
/// Custom data: a single frame with waveform strength pinned at the maximum of
/// 100, used to give a steady full output during bench testing, without the
/// rising and falling envelope of breathing / tide.
pub const FULL: Builtin = Builtin {
    name: "full",
    label: "Full power",
    official: false,
    frames: &[(10, 100)],
};

/// Staccato: full power and silence alternating every 100ms, very grainy.
///
/// Custom data.
pub const STACCATO: Builtin = Builtin {
    name: "staccato",
    label: "Staccato",
    official: false,
    frames: &[
        (10, 100),
        (10, 0),
        (10, 100),
        (10, 0),
        (10, 100),
        (10, 0),
        (10, 0),
        (10, 0),
    ],
};

/// Heartbeat: two hard hits plus a stretch of silence, mimicking a heart rhythm.
///
/// Custom data.
pub const PULSE: Builtin = Builtin {
    name: "pulse",
    label: "Heartbeat",
    official: false,
    frames: &[
        (12, 95),
        (12, 40),
        (12, 95),
        (12, 0),
        (12, 0),
        (12, 0),
        (12, 0),
        (12, 0),
        (12, 0),
        (12, 0),
    ],
};

/// Ramp: climbs smoothly from zero to full, then zeroes out and starts over.
///
/// Custom data.
pub const RAMP: Builtin = Builtin {
    name: "ramp",
    label: "Ramp",
    official: false,
    frames: &[
        (15, 0),
        (15, 8),
        (15, 16),
        (15, 24),
        (15, 32),
        (15, 40),
        (15, 48),
        (15, 56),
        (15, 64),
        (15, 72),
        (15, 80),
        (15, 88),
        (15, 96),
        (15, 100),
        (15, 100),
        (15, 0),
    ],
};

/// Steps: strength jumps between levels, holding each one for 200ms.
///
/// Custom data.
pub const STEPS: Builtin = Builtin {
    name: "steps",
    label: "Steps",
    official: false,
    frames: &[
        (20, 20),
        (20, 20),
        (20, 45),
        (20, 45),
        (20, 70),
        (20, 70),
        (20, 100),
        (20, 100),
        (20, 0),
        (20, 0),
    ],
};

/// Flutter: high frequency and finely grained, with small strength swings.
///
/// Custom data. The frequency sits in the protocol's upper band (120-160), so it
/// feels different from the low-frequency, impact-heavy waveforms.
pub const FLUTTER: Builtin = Builtin {
    name: "flutter",
    label: "Flutter",
    official: false,
    frames: &[(120, 55), (140, 70), (160, 60), (140, 45), (120, 65)],
};

/// All built-in waveforms.
pub const ALL: &[Builtin] = &[BREATHING, TIDE, FULL, STACCATO, PULSE, RAMP, STEPS, FLUTTER];

/// Look up a built-in waveform by name.
pub fn by_name(name: &str) -> Option<&'static Builtin> {
    ALL.iter().find(|w| w.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_within_protocol_range() {
        for wave in ALL {
            for &(frequency, intensity) in wave.frames {
                Pulse::new(frequency, intensity).unwrap_or_else(|e| {
                    panic!(
                        "frame ({frequency}, {intensity}) of waveform {} is out of range: {e}",
                        wave.name
                    )
                });
            }
        }
    }

    #[test]
    fn lookup_and_cycling() {
        let tide = by_name("TIDE").unwrap();
        assert_eq!(tide.label, "Tide");
        // an index past the frame count wraps back to the first frame
        assert_eq!(tide.pulses_at(0), tide.pulses_at(tide.frames.len()));
        assert_eq!(BREATHING.cycle_ms(), 1200);
        assert!(by_name("nope").is_none());
    }
}
