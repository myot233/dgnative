//! Coyote pulse host V2 (Coyote 2.0) Bluetooth protocol.
//!
//! V2 drives output through three 3-byte characteristics: [`CHAR_STRENGTH`]
//! (PWM_AB2, strength of both channels), [`CHAR_WAVE_A`] (PWM_A34) and
//! [`CHAR_WAVE_B`] (PWM_B34, channel waveform). Each set of waveform
//! parameters is only valid for 0.1s, so continuous output requires
//! rewriting the waveform characteristic every 100ms.
//!
//! All 3-byte values are the packed 24-bit integer sent in **little-endian**
//! order (verified against the HEX data in the official example.md).
//!
//! > Note: in the official documentation tables the "description" column of
//! > PWM_A34 / PWM_B34 lists channel B / A swapped, while their bit
//! > definitions are named Ax/Ay/Az and Bx/By/Bz respectively. This
//! > implementation follows the characteristic names and bit definitions
//! > (0x1505 = channel A, 0x1506 = channel B); if real hardware behaves the
//! > other way round, just swap the writes to the two characteristics.

use crate::{Error, Result};
use uuid::Uuid;

/// Bluetooth advertised name of the pulse host 2.0.
pub const BLE_NAME: &str = "D-LAB ESTIM01";

/// Build the DG-LAB V2 private base UUID from a 16-bit short UUID:
/// `955Axxxx-0FE2-F5AA-A094-84B8D4F3E8AD`
pub const fn dglab_uuid(short: u16) -> Uuid {
    Uuid::from_u128(0x955A_0000_0FE2_F5AA_A094_84B8_D4F3_E8AD | ((short as u128) << 96))
}

/// Battery service (0x180A; note this uses the 955A base UUID, not the Bluetooth standard base UUID).
pub const SERVICE_BATTERY: Uuid = dglab_uuid(0x180A);
/// Battery characteristic (0x1500), read / notify, 1-byte integer 0-100.
pub const CHAR_BATTERY: Uuid = dglab_uuid(0x1500);
/// Pulse service (0x180B).
pub const SERVICE_ESTIM: Uuid = dglab_uuid(0x180B);
/// PWM_AB2 (0x1504): strength of both channels A and B, read / write / notify, 3 bytes.
pub const CHAR_STRENGTH: Uuid = dglab_uuid(0x1504);
/// PWM_A34 (0x1505): channel A waveform data, read / write, 3 bytes.
pub const CHAR_WAVE_A: Uuid = dglab_uuid(0x1505);
/// PWM_B34 (0x1506): channel B waveform data, read / write, 3 bytes.
pub const CHAR_WAVE_B: Uuid = dglab_uuid(0x1506);

/// Upper bound of the actual per-channel strength (11 bits). Every +1 of the
/// value displayed in the official app is +7 of actual strength
/// (actual strength = displayed value x 7).
pub const MAX_STRENGTH: u16 = 2047;
/// Recommended rewrite interval for the waveform characteristics.
pub const WAVE_INTERVAL_MS: u64 = 100;
/// Conversion factor between the strength displayed in the official app and the actual strength.
pub const STRENGTH_PER_APP_LEVEL: u16 = 7;

/// Encode a PWM_AB2 strength value: `(A << 11) | B` as 24-bit little-endian.
///
/// A and B range over 0..=[`MAX_STRENGTH`].
pub fn encode_strength(a: u16, b: u16) -> Result<[u8; 3]> {
    for (field, v) in [("strength_a", a), ("strength_b", b)] {
        if v > MAX_STRENGTH {
            return Err(Error::OutOfRange {
                field,
                value: v as u32,
                min: 0,
                max: MAX_STRENGTH as u32,
            });
        }
    }
    let packed = (a as u32) << 11 | b as u32;
    Ok([packed as u8, (packed >> 8) as u8, (packed >> 16) as u8])
}

/// Decode a PWM_AB2 strength value, returning `(A, B)`.
pub const fn decode_strength(bytes: [u8; 3]) -> (u16, u16) {
    let packed = bytes[0] as u32 | (bytes[1] as u32) << 8 | (bytes[2] as u32) << 16;
    (((packed >> 11) & 0x7FF) as u16, (packed & 0x7FF) as u16)
}

/// One set of waveform parameters [X, Y, Z], valid for 100ms.
///
/// - X (0..=31): emit X pulses over X consecutive milliseconds;
/// - Y (0..=1023): after those X pulses, wait Y milliseconds, then repeat;
/// - Z (0..=31): pulse width; the actual width is Z x 5us (Z > 20 stings more easily).
///
/// The perceived frequency characteristic value is `Frequency = X + Y`
/// (milliseconds); the real frequency is `1000 / Frequency` Hz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Waveform {
    /// Number of consecutive pulses / milliseconds (0..=31).
    pub x: u8,
    /// Gap between pulse groups, in milliseconds (0..=1023).
    pub y: u16,
    /// Pulse width parameter (0..=31; actual width is Z x 5us).
    pub z: u8,
}

impl Waveform {
    /// Build a range-checked set of waveform parameters.
    pub fn new(x: u8, y: u16, z: u8) -> Result<Self> {
        if x > 31 {
            return Err(Error::OutOfRange {
                field: "x",
                value: x as u32,
                min: 0,
                max: 31,
            });
        }
        if y > 1023 {
            return Err(Error::OutOfRange {
                field: "y",
                value: y as u32,
                min: 0,
                max: 1023,
            });
        }
        if z > 31 {
            return Err(Error::OutOfRange {
                field: "z",
                value: z as u32,
                min: 0,
                max: 31,
            });
        }
        Ok(Waveform { x, y, z })
    }

    /// Derive X and Y from the Frequency characteristic value (10..=1000)
    /// using the officially recommended formula:
    ///
    /// ```text
    /// X = ⌊((Frequency / 1000)^0.5) × 15⌋
    /// Y = Frequency − X
    /// ```
    pub fn from_frequency(frequency: u16, z: u8) -> Result<Self> {
        if !(10..=1000).contains(&frequency) {
            return Err(Error::OutOfRange {
                field: "frequency",
                value: frequency as u32,
                min: 10,
                max: 1000,
            });
        }
        let x = ((frequency as f64 / 1000.0).sqrt() * 15.0) as u8;
        Waveform::new(x, frequency - x as u16, z)
    }

    /// Encode into 3 bytes: `(Z << 15) | (Y << 5) | X` as 24-bit little-endian.
    pub fn encode(&self) -> [u8; 3] {
        let packed = (self.z as u32) << 15 | (self.y as u32) << 5 | self.x as u32;
        [packed as u8, (packed >> 8) as u8, (packed >> 16) as u8]
    }

    /// Decode waveform parameters from 3 bytes.
    pub const fn decode(bytes: [u8; 3]) -> Waveform {
        let packed = bytes[0] as u32 | (bytes[1] as u32) << 8 | (bytes[2] as u32) << 16;
        Waveform {
            x: (packed & 0x1F) as u8,
            y: ((packed >> 5) & 0x3FF) as u16,
            z: ((packed >> 15) & 0x1F) as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    // Test vectors taken from the official docs, coyote/v2/example.md

    #[test]
    fn waveform_encoding_matches_official_examples() {
        for (x, y, z, expected) in [
            (1u8, 9u16, 0u8, "210100"),
            (1, 9, 4, "210102"),
            (1, 9, 20, "21010A"),
            (1, 10, 3, "418101"),
            (1, 15, 13, "E18106"),
            (1, 34, 20, "41040A"),
            (1, 41, 13, "218506"),
        ] {
            let w = Waveform::new(x, y, z).unwrap();
            assert_eq!(hex(&w.encode()), expected, "x={x} y={y} z={z}");
            assert_eq!(Waveform::decode(w.encode()), w);
        }
    }

    #[test]
    fn strength_round_trip() {
        for (a, b) in [(0, 0), (7, 7), (2047, 0), (0, 2047), (700, 1234)] {
            let bytes = encode_strength(a, b).unwrap();
            assert_eq!(decode_strength(bytes), (a, b));
        }
        assert!(encode_strength(2048, 0).is_err());
        assert!(encode_strength(0, 2048).is_err());
    }

    #[test]
    fn frequency_formula() {
        // Breathing waveform from the official docs: Frequency=10 -> [1, 9]
        let w = Waveform::from_frequency(10, 0).unwrap();
        assert_eq!((w.x, w.y), (1, 9));
        // Frequency=1000 -> X=15, Y=985
        let w = Waveform::from_frequency(1000, 0).unwrap();
        assert_eq!((w.x, w.y), (15, 985));
        assert!(Waveform::from_frequency(9, 0).is_err());
        assert!(Waveform::from_frequency(1001, 0).is_err());
    }

    #[test]
    fn waveform_validation() {
        assert!(Waveform::new(32, 0, 0).is_err());
        assert!(Waveform::new(0, 1024, 0).is_err());
        assert!(Waveform::new(0, 0, 32).is_err());
    }

    #[test]
    fn uuids() {
        assert_eq!(
            CHAR_STRENGTH.to_string(),
            "955a1504-0fe2-f5aa-a094-84b8d4f3e8ad"
        );
        assert_eq!(
            SERVICE_ESTIM.to_string(),
            "955a180b-0fe2-f5aa-a094-84b8d4f3e8ad"
        );
    }
}
