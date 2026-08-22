//! Pure protocol codec layer, no IO.
//!
//! - [`v3`]: Coyote 3.0 (Bluetooth name `47L121000`), B0 / BF commands and B1 reply messages;
//! - [`v2`]: Coyote 2.0 (Bluetooth name `D-LAB ESTIM01`), PWM_AB2 strength and PWM_A34 / PWM_B34 waveforms.

pub mod v2;
pub mod v3;

use uuid::Uuid;

/// Build the Bluetooth standard base UUID from a 16-bit short UUID:
/// `0000xxxx-0000-1000-8000-00805f9b34fb`
pub const fn sig_uuid(short: u16) -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_1000_8000_0080_5f9b_34fb | ((short as u128) << 96))
}
