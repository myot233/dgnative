//! Coyote pulse host V3 (Coyote 3.0) Bluetooth protocol.
//!
//! All commands are written to characteristic [`CHAR_WRITE`] (0x150A), and all
//! reply messages come back through notifications on [`CHAR_NOTIFY`] (0x150B).
//! Unlike V2, V3 data needs no endianness conversion.
//!
//! Core rhythm: the host writes one 20-byte [`B0`] command every **100ms**,
//! carrying 4 groups (25ms each) of waveform frequency / waveform strength for
//! both channels, plus an optional channel strength change.

pub mod builtin;

use crate::protocol::sig_uuid;
use crate::{Error, Result};
use uuid::Uuid;

/// Bluetooth advertising name of the pulse host 3.0.
pub const BLE_NAME: &str = "47L121000";
/// Bluetooth advertising name of the wireless sensor.
pub const BLE_NAME_WIRELESS_SENSOR: &str = "47L120100";

/// Main service (0x180C); both command writes and message notifications live
/// under this service.
pub const SERVICE_MAIN: Uuid = sig_uuid(0x180C);
/// Write characteristic (0x150A); all commands are written here, 20 bytes max.
pub const CHAR_WRITE: Uuid = sig_uuid(0x150A);
/// Notify characteristic (0x150B); all reply messages come back here, 20 bytes
/// max.
pub const CHAR_NOTIFY: Uuid = sig_uuid(0x150B);
/// Battery service (0x180A).
pub const SERVICE_BATTERY: Uuid = sig_uuid(0x180A);
/// Battery characteristic (0x1500); read / notify, a 1-byte integer 0-100.
pub const CHAR_BATTERY: Uuid = sig_uuid(0x1500);

/// Absolute upper bound of channel strength (0..=200).
pub const MAX_STRENGTH: u8 = 200;
/// Lower bound of the valid waveform frequency range.
pub const FREQ_MIN: u8 = 10;
/// Upper bound of the valid waveform frequency range.
pub const FREQ_MAX: u8 = 240;
/// Upper bound of the valid waveform strength range.
pub const INTENSITY_MAX: u8 = 100;
/// Recommended write interval for the B0 command.
pub const B0_INTERVAL_MS: u64 = 100;

/// Convert an intuitive frequency input in the range (10 ~ 1000) into the
/// waveform frequency byte (10 ~ 240) that the B0 command requires, using the
/// officially recommended algorithm.
///
/// Inputs outside 10 ~ 1000 return 10, per the official algorithm.
pub const fn encode_frequency(input: u16) -> u8 {
    match input {
        10..=100 => input as u8,
        101..=600 => ((input - 100) / 5 + 100) as u8,
        601..=1000 => ((input - 600) / 10 + 200) as u8,
        _ => 10,
    }
}

/// One 25ms group of waveform data: frequency + strength.
///
/// Valid frequency range is [`FREQ_MIN`]..=[`FREQ_MAX`], valid strength range is
/// 0..=[`INTENSITY_MAX`]. The fields are public raw bytes: the protocol states
/// that if an invalid value appears in any of a channel's 4 groups, the pulse
/// host **discards all 4 groups of that channel for the whole command** (the
/// official docs use exactly this - one invalid strength value - to keep a
/// single channel silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pulse {
    /// Waveform frequency (valid range 10..=240).
    pub frequency: u8,
    /// Waveform strength (valid range 0..=100).
    pub intensity: u8,
}

impl Pulse {
    /// A valid waveform with no output (frequency 10, strength 0).
    pub const STOP: Pulse = Pulse {
        frequency: 10,
        intensity: 0,
    };

    /// Build a range-checked group of waveform data.
    pub fn new(frequency: u8, intensity: u8) -> Result<Self> {
        if !(FREQ_MIN..=FREQ_MAX).contains(&frequency) {
            return Err(Error::OutOfRange {
                field: "frequency",
                value: frequency as u32,
                min: FREQ_MIN as u32,
                max: FREQ_MAX as u32,
            });
        }
        if intensity > INTENSITY_MAX {
            return Err(Error::OutOfRange {
                field: "intensity",
                value: intensity as u32,
                min: 0,
                max: INTENSITY_MAX as u32,
            });
        }
        Ok(Pulse {
            frequency,
            intensity,
        })
    }

    /// Build waveform data from an intuitive frequency input in (10 ~ 1000);
    /// the frequency is converted by [`encode_frequency`].
    pub fn from_freq_input(input: u16, intensity: u8) -> Result<Self> {
        Pulse::new(encode_frequency(input), intensity)
    }
}

impl Default for Pulse {
    fn default() -> Self {
        Pulse::STOP
    }
}

/// How channel strength is interpreted in a B0 command (2 bits + a 1-byte
/// setting value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrengthAction {
    /// 0b00: leave this channel's strength unchanged (setting value ignored).
    #[default]
    Keep,
    /// 0b01: increase by the given value, relative.
    Increase(u8),
    /// 0b10: decrease by the given value, relative.
    Decrease(u8),
    /// 0b11: set absolutely to the given value.
    Set(u8),
}

impl StrengthAction {
    const fn mode_bits(self) -> u8 {
        match self {
            StrengthAction::Keep => 0b00,
            StrengthAction::Increase(_) => 0b01,
            StrengthAction::Decrease(_) => 0b10,
            StrengthAction::Set(_) => 0b11,
        }
    }

    const fn setting_value(self) -> u8 {
        match self {
            StrengthAction::Keep => 0,
            StrengthAction::Increase(v) | StrengthAction::Decrease(v) | StrengthAction::Set(v) => v,
        }
    }

    /// Check that the setting value does not exceed [`MAX_STRENGTH`].
    ///
    /// The protocol states that setting values outside 0..=200 are all treated
    /// as 0 (i.e. they fail silently); encoding itself will not reject them, so
    /// callers can use this method to catch it up front.
    pub fn validate(self) -> Result<()> {
        let v = self.setting_value();
        if v > MAX_STRENGTH {
            return Err(Error::OutOfRange {
                field: "strength",
                value: v as u32,
                min: 0,
                max: MAX_STRENGTH as u32,
            });
        }
        Ok(())
    }
}

/// B0 command: channel strength change + 4 groups of waveform data for each of
/// the two channels, 20 bytes total, written once every 100ms.
///
/// If the command modifies channel strength and you want the device to report
/// the result, set [`sequence`] to 1..=15; the device replies with the modified
/// strength in a [`B1`] message carrying the same sequence number. Official
/// advice: after sending a strength change with a non-zero sequence number, wait
/// for the B1 with that same sequence number before making the next strength
/// change (see [`StrengthQueue`]).
///
/// [`sequence`]: B0::sequence
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct B0 {
    /// Sequence number (0..=15; the high bits are truncated when encoding). 0
    /// means no strength report is needed from the device.
    pub sequence: u8,
    /// Strength action for channel A.
    pub action_a: StrengthAction,
    /// Strength action for channel B.
    pub action_b: StrengthAction,
    /// 4 waveform groups for channel A (25ms each, 100ms total).
    pub pulses_a: [Pulse; 4],
    /// 4 waveform groups for channel B (25ms each, 100ms total).
    pub pulses_b: [Pulse; 4],
}

impl B0 {
    /// Command HEAD byte.
    pub const HEAD: u8 = 0xB0;

    /// Encode into a 20-byte command.
    ///
    /// Layout: `0xB0 | sequence(4b)+interpretation(4b) | A strength |
    /// B strength | A frequency x4 | A strength x4 | B frequency x4 |
    /// B strength x4`, where the high 2 bits of the interpretation field are
    /// channel A and the low 2 bits are channel B.
    pub fn encode(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0] = Self::HEAD;
        buf[1] = (self.sequence & 0x0F) << 4
            | self.action_a.mode_bits() << 2
            | self.action_b.mode_bits();
        buf[2] = self.action_a.setting_value();
        buf[3] = self.action_b.setting_value();
        for i in 0..4 {
            buf[4 + i] = self.pulses_a[i].frequency;
            buf[8 + i] = self.pulses_a[i].intensity;
            buf[12 + i] = self.pulses_b[i].frequency;
            buf[16 + i] = self.pulses_b[i].intensity;
        }
        buf
    }
}

/// BF command: channel soft strength limits + frequency balance parameters +
/// strength balance parameters, 7 bytes total.
///
/// ⚠️ The official docs stress that BF takes effect immediately once written and
/// **returns nothing**, so it must be rewritten after every reconnect to the
/// device to avoid leaving an unexpected soft limit value behind. All three
/// parameter pairs persist across power cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bf {
    /// Soft strength limit for channel A (0..=200; values out of range leave
    /// the soft limit untouched).
    pub limit_a: u8,
    /// Soft strength limit for channel B (0..=200).
    pub limit_b: u8,
    /// Frequency balance parameter 1 for channel A (0..=255; higher means
    /// low-frequency waveforms hit harder).
    pub freq_balance_a: u8,
    /// Frequency balance parameter 1 for channel B.
    pub freq_balance_b: u8,
    /// Frequency balance parameter 2 / strength balance parameter for channel A
    /// (0..=255; higher means low-frequency waveforms stimulate more strongly).
    pub intensity_balance_a: u8,
    /// Frequency balance parameter 2 / strength balance parameter for channel B.
    pub intensity_balance_b: u8,
}

impl Bf {
    /// Command HEAD byte.
    pub const HEAD: u8 = 0xBF;

    /// Default frequency balance parameter used by the official App.
    pub const DEFAULT_FREQ_BALANCE: u8 = 160;
    /// Default strength balance parameter used by the official App.
    pub const DEFAULT_INTENSITY_BALANCE: u8 = 0;

    /// Set only the soft limits, leaving the balance parameters at their
    /// defaults.
    pub const fn with_limits(limit_a: u8, limit_b: u8) -> Self {
        Bf {
            limit_a,
            limit_b,
            freq_balance_a: Self::DEFAULT_FREQ_BALANCE,
            freq_balance_b: Self::DEFAULT_FREQ_BALANCE,
            intensity_balance_a: Self::DEFAULT_INTENSITY_BALANCE,
            intensity_balance_b: Self::DEFAULT_INTENSITY_BALANCE,
        }
    }

    /// Encode into a 7-byte command: `0xBF | A limit | B limit |
    /// A frequency balance | B frequency balance | A strength balance |
    /// B strength balance`.
    pub fn encode(&self) -> [u8; 7] {
        [
            Self::HEAD,
            self.limit_a,
            self.limit_b,
            self.freq_balance_a,
            self.freq_balance_b,
            self.intensity_balance_a,
            self.intensity_balance_b,
        ]
    }
}

/// B1 reply message: sent immediately over 0x150B whenever device strength
/// changes.
///
/// If the change was caused by a B0 command, `sequence` matches that command's
/// sequence number; if it was caused by a physical action such as the dial,
/// `sequence` is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct B1 {
    /// Sequence number of the B0 that caused this change; 0 means it was not
    /// caused by a B0.
    pub sequence: u8,
    /// Current actual strength of channel A.
    pub strength_a: u8,
    /// Current actual strength of channel B.
    pub strength_b: u8,
}

impl B1 {
    /// Message HEAD byte.
    pub const HEAD: u8 = 0xB1;
}

/// A reply message returned by the 0x150B characteristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// B1 strength report.
    Strength(B1),
    /// Unknown / undocumented message (such as the deprecated BE), kept as-is.
    Unknown(Vec<u8>),
}

/// Parse one message returned by the 0x150B characteristic.
pub fn parse_notification(data: &[u8]) -> Result<Notification> {
    match data {
        [] => Err(Error::Parse("empty message".into())),
        [B1::HEAD, sequence, strength_a, strength_b, ..] => Ok(Notification::Strength(B1 {
            sequence: *sequence,
            strength_a: *strength_a,
            strength_b: *strength_b,
        })),
        [B1::HEAD, ..] => Err(Error::Parse(format!("B1 message too short: {data:02X?}"))),
        _ => Ok(Notification::Unknown(data.to_vec())),
    }
}

/// A strength change waiting to be written (internal state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pending {
    #[default]
    None,
    /// Accumulated relative change.
    Delta(i32),
    /// Absolute set (later relative changes accumulate on top of it).
    Absolute(i32),
}

impl Pending {
    fn adjust(&mut self, delta: i32) {
        *self = match *self {
            Pending::None => Pending::Delta(delta),
            Pending::Delta(d) => Pending::Delta(d + delta),
            Pending::Absolute(v) => Pending::Absolute(v + delta),
        };
    }

    fn to_action(self) -> StrengthAction {
        match self {
            Pending::None | Pending::Delta(0) => StrengthAction::Keep,
            Pending::Delta(d) if d > 0 => {
                StrengthAction::Increase(d.min(MAX_STRENGTH as i32) as u8)
            }
            Pending::Delta(d) => StrengthAction::Decrease((-d).min(MAX_STRENGTH as i32) as u8),
            Pending::Absolute(v) => StrengthAction::Set(v.clamp(0, MAX_STRENGTH as i32) as u8),
        }
    }
}

/// The strength input state machine recommended by the official docs.
///
/// The protocol convention: after sending a strength change with a non-zero
/// sequence number, wait until the device reports a B1 message with the same
/// sequence number before issuing another strength change; new user increments
/// and decrements accumulate while waiting. This struct wraps that rhythm into
/// three call sites:
///
/// - call [`adjust_a`] / [`adjust_b`] / [`set_a`] / [`set_b`] on user action;
/// - call [`tick`] before assembling each B0 every 100ms, to get the sequence
///   number and actions to fill in;
/// - call [`on_b1`] when a B1 message arrives.
///
/// If the device does not report a B1 because strength did not actually change
/// (for example the soft limit is already reached), the state machine releases
/// the wait automatically after a few ticks to avoid a deadlock.
///
/// [`adjust_a`]: StrengthQueue::adjust_a
/// [`adjust_b`]: StrengthQueue::adjust_b
/// [`set_a`]: StrengthQueue::set_a
/// [`set_b`]: StrengthQueue::set_b
/// [`tick`]: StrengthQueue::tick
/// [`on_b1`]: StrengthQueue::on_b1
#[derive(Debug, Clone)]
pub struct StrengthQueue {
    pending_a: Pending,
    pending_b: Pending,
    next_seq: u8,
    inflight: Option<(u8, u32)>,
    timeout_ticks: u32,
}

impl Default for StrengthQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl StrengthQueue {
    /// Create a new state machine whose B1 wait times out after 10 ticks
    /// (about 1 second).
    pub fn new() -> Self {
        StrengthQueue {
            pending_a: Pending::None,
            pending_b: Pending::None,
            next_seq: 1,
            inflight: None,
            timeout_ticks: 10,
        }
    }

    /// Accumulate a relative strength change for channel A.
    pub fn adjust_a(&mut self, delta: i32) {
        self.pending_a.adjust(delta);
    }

    /// Accumulate a relative strength change for channel B.
    pub fn adjust_b(&mut self, delta: i32) {
        self.pending_b.adjust(delta);
    }

    /// Request that channel A strength be set absolutely to `value`
    /// (discarding any previously accumulated change).
    pub fn set_a(&mut self, value: u8) {
        self.pending_a = Pending::Absolute(value as i32);
    }

    /// Request that channel B strength be set absolutely to `value`
    /// (discarding any previously accumulated change).
    pub fn set_b(&mut self, value: u8) {
        self.pending_b = Pending::Absolute(value as i32);
    }

    /// Immediately request that both channels zero out, abandoning the wait for
    /// any in-flight B1.
    ///
    /// This corresponds to `strengthZero()` in the official docs: zeroing out is
    /// **not** throttled by the "wait for B1 confirmation" rule, so the very
    /// next [`tick`] sends it. Emergency stop should go through here; with
    /// [`set_a`]`(0)` it would, worst case, take a confirmation timeout (about
    /// 1 second) to take effect.
    ///
    /// [`tick`]: StrengthQueue::tick
    /// [`set_a`]: StrengthQueue::set_a
    pub fn zero_now(&mut self) {
        self.pending_a = Pending::Absolute(0);
        self.pending_b = Pending::Absolute(0);
        self.inflight = None;
    }

    /// Whether any strength change is still unwritten or unconfirmed.
    pub fn is_idle(&self) -> bool {
        self.inflight.is_none()
            && self.pending_a.to_action() == StrengthAction::Keep
            && self.pending_b.to_action() == StrengthAction::Keep
    }

    /// Call once before assembling each B0 command every 100ms; returns the
    /// `(sequence number, channel A action, channel B action)` to fill in.
    pub fn tick(&mut self) -> (u8, StrengthAction, StrengthAction) {
        if let Some((_, ticks)) = &mut self.inflight {
            *ticks += 1;
            if *ticks < self.timeout_ticks {
                // no new strength change while waiting for B1 confirmation
                return (0, StrengthAction::Keep, StrengthAction::Keep);
            }
            self.inflight = None;
        }

        let action_a = self.pending_a.to_action();
        let action_b = self.pending_b.to_action();
        if action_a == StrengthAction::Keep && action_b == StrengthAction::Keep {
            return (0, StrengthAction::Keep, StrengthAction::Keep);
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq % 15 + 1;
        self.inflight = Some((seq, 0));
        self.pending_a = Pending::None;
        self.pending_b = Pending::None;
        (seq, action_a, action_b)
    }

    /// Call when a B1 message arrives; the wait is released when the sequence
    /// number matches the in-flight command.
    pub fn on_b1(&mut self, b1: &B1) {
        if let Some((seq, _)) = self.inflight
            && b1.sequence == seq
        {
            self.inflight = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    fn pulses(freqs: [u8; 4], intensities: [u8; 4]) -> [Pulse; 4] {
        core::array::from_fn(|i| Pulse {
            frequency: freqs[i],
            intensity: intensities[i],
        })
    }

    // every test vector below comes from the examples in the official doc
    // coyote/v3/README.md

    #[test]
    fn b0_no_strength_change_channel_a_only() {
        let cmd = B0 {
            sequence: 0,
            action_a: StrengthAction::Keep,
            action_b: StrengthAction::Keep,
            pulses_a: pulses([10, 10, 10, 10], [0, 10, 20, 30]),
            pulses_b: pulses([0, 0, 0, 0], [0, 0, 0, 101]),
        };
        assert_eq!(
            hex(&cmd.encode()),
            "B00000000A0A0A0A000A141E0000000000000065"
        );
    }

    #[test]
    fn b0_relative_increase_without_feedback() {
        let cmd = B0 {
            sequence: 0,
            action_a: StrengthAction::Increase(5),
            action_b: StrengthAction::Keep,
            pulses_a: pulses([10, 10, 10, 10], [0, 10, 20, 30]),
            pulses_b: pulses([0, 0, 0, 0], [0, 0, 0, 101]),
        };
        assert_eq!(
            hex(&cmd.encode()),
            "B00405000A0A0A0A000A141E0000000000000065"
        );
    }

    #[test]
    fn b0_relative_increase_with_sequence() {
        let cmd = B0 {
            sequence: 1,
            action_a: StrengthAction::Increase(10),
            action_b: StrengthAction::Keep,
            pulses_a: pulses([40, 60, 80, 100], [100, 90, 90, 90]),
            pulses_b: pulses([0, 0, 0, 0], [0, 0, 0, 101]),
        };
        assert_eq!(
            hex(&cmd.encode()),
            "B0140A00283C5064645A5A5A0000000000000065"
        );
    }

    #[test]
    fn b0_both_channels() {
        let cmd = B0 {
            pulses_a: pulses([15, 15, 15, 15], [40, 50, 60, 70]),
            pulses_b: pulses([10, 10, 10, 10], [10, 10, 10, 10]),
            ..B0::default()
        };
        assert_eq!(
            hex(&cmd.encode()),
            "B00000000F0F0F0F28323C460A0A0A0A0A0A0A0A"
        );
    }

    #[test]
    fn frequency_conversion_matches_official_algorithm() {
        assert_eq!(encode_frequency(9), 10); // out of range
        assert_eq!(encode_frequency(10), 10);
        assert_eq!(encode_frequency(100), 100);
        assert_eq!(encode_frequency(101), 100); // (101-100)/5+100
        assert_eq!(encode_frequency(600), 200);
        assert_eq!(encode_frequency(601), 200); // (601-600)/10+200
        assert_eq!(encode_frequency(1000), 240);
        assert_eq!(encode_frequency(1001), 10); // out of range
    }

    #[test]
    fn bf_encoding() {
        let bf = Bf {
            limit_a: 150,
            limit_b: 30,
            freq_balance_a: 160,
            freq_balance_b: 160,
            intensity_balance_a: 0,
            intensity_balance_b: 0,
        };
        assert_eq!(bf.encode(), [0xBF, 150, 30, 160, 160, 0, 0]);
    }

    #[test]
    fn parse_b1() {
        let n = parse_notification(&[0xB1, 1, 25, 0]).unwrap();
        assert_eq!(
            n,
            Notification::Strength(B1 {
                sequence: 1,
                strength_a: 25,
                strength_b: 0
            })
        );
    }

    #[test]
    fn parse_unknown_and_invalid() {
        assert!(matches!(
            parse_notification(&[0xBE, 1, 2]),
            Ok(Notification::Unknown(_))
        ));
        assert!(parse_notification(&[]).is_err());
        assert!(parse_notification(&[0xB1, 1]).is_err());
    }

    #[test]
    fn pulse_validation() {
        assert!(Pulse::new(10, 0).is_ok());
        assert!(Pulse::new(240, 100).is_ok());
        assert!(Pulse::new(9, 0).is_err());
        assert!(Pulse::new(241, 0).is_err());
        assert!(Pulse::new(10, 101).is_err());
    }

    #[test]
    fn strength_queue_waits_for_b1() {
        let mut q = StrengthQueue::new();
        q.adjust_a(1);
        // first tick: sends the +1 with seq=1
        assert_eq!(
            q.tick(),
            (1, StrengthAction::Increase(1), StrengthAction::Keep)
        );
        // keep accumulating while waiting for confirmation
        q.adjust_a(3);
        assert_eq!(q.tick(), (0, StrengthAction::Keep, StrengthAction::Keep));
        // after the B1 confirmation the next tick sends the accumulated +3, with
        // an incremented sequence number
        q.on_b1(&B1 {
            sequence: 1,
            strength_a: 1,
            strength_b: 0,
        });
        assert_eq!(
            q.tick(),
            (2, StrengthAction::Increase(3), StrengthAction::Keep)
        );
    }

    #[test]
    fn strength_queue_set_overrides_pending_delta() {
        let mut q = StrengthQueue::new();
        q.adjust_a(5);
        q.set_a(0);
        q.adjust_b(-2);
        assert_eq!(
            q.tick(),
            (1, StrengthAction::Set(0), StrengthAction::Decrease(2))
        );
    }

    #[test]
    fn zero_now_bypasses_the_b1_wait() {
        let mut q = StrengthQueue::new();
        q.adjust_a(5);
        assert_eq!(q.tick().0, 1); // sent, now waiting
        assert_eq!(q.tick(), (0, StrengthAction::Keep, StrengthAction::Keep));

        // emergency stop does not wait for B1: the next tick zeroes out both
        // channels
        q.zero_now();
        assert_eq!(
            q.tick(),
            (2, StrengthAction::Set(0), StrengthAction::Set(0))
        );
    }

    #[test]
    fn strength_queue_times_out_without_b1() {
        let mut q = StrengthQueue::new();
        q.adjust_a(1);
        assert_eq!(q.tick().0, 1);
        q.adjust_a(1);
        // stays Keep until the timeout
        for _ in 0..9 {
            assert_eq!(q.tick().0, 0);
        }
        // after the timeout the wait is released and the accumulated change goes
        // out
        assert_eq!(
            q.tick(),
            (2, StrengthAction::Increase(1), StrengthAction::Keep)
        );
    }

    #[test]
    fn strength_queue_sequence_wraps_within_4_bits() {
        let mut q = StrengthQueue::new();
        let mut last = 0;
        for _ in 0..20 {
            q.adjust_a(1);
            let (seq, _, _) = q.tick();
            assert!((1..=15).contains(&seq));
            q.on_b1(&B1 {
                sequence: seq,
                strength_a: 0,
                strength_b: 0,
            });
            last = seq;
        }
        // the sequence number cycles within 1..=15, so the 20th should be 5
        assert_eq!(last, 5);
    }

    #[test]
    fn uuids() {
        assert_eq!(
            SERVICE_MAIN.to_string(),
            "0000180c-0000-1000-8000-00805f9b34fb"
        );
        assert_eq!(
            CHAR_WRITE.to_string(),
            "0000150a-0000-1000-8000-00805f9b34fb"
        );
    }
}
