//! Coyote 2.0 BLE client.

use crate::protocol::v2::{
    self, CHAR_BATTERY, CHAR_STRENGTH, CHAR_WAVE_A, CHAR_WAVE_B, Waveform, decode_strength,
    encode_strength,
};
use crate::{Error, Result};
use btleplug::api::{Characteristic, Peripheral as _, WriteType};
use btleplug::platform::{Adapter, Peripheral};
use futures::StreamExt;
use futures::stream::BoxStream;
use std::time::Duration;

use super::{default_adapter, find_by_name, preferred_write_type, require_characteristic};

/// Event returned by [`Coyote2::events`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coyote2Event {
    /// Actual strength `(A, B)` of both channels, from a PWM_AB2 notification.
    Strength { a: u16, b: u16 },
    /// Battery change notification (0-100).
    Battery(u8),
}

/// Coyote 2.0 (Bluetooth name `D-LAB ESTIM01`) client.
///
/// Once written to PWM_AB2 the strength is held indefinitely; each set of
/// waveform parameters is only valid for 0.1s, so [`set_waveform_a`] /
/// [`set_waveform_b`] must be called every 100ms for continuous output.
///
/// [`set_waveform_a`]: Coyote2::set_waveform_a
/// [`set_waveform_b`]: Coyote2::set_waveform_b
pub struct Coyote2 {
    peripheral: Peripheral,
    strength_char: Characteristic,
    wave_a_char: Characteristic,
    wave_b_char: Characteristic,
    battery_char: Option<Characteristic>,
    write_type: WriteType,
}

impl Coyote2 {
    /// Scan with the system's first Bluetooth adapter and connect to the first Coyote 2.0.
    pub async fn scan_and_connect(timeout: Duration) -> Result<Self> {
        let adapter = default_adapter().await?;
        Self::scan_and_connect_with(&adapter, timeout).await
    }

    /// Scan with the given adapter and connect to the first Coyote 2.0.
    pub async fn scan_and_connect_with(adapter: &Adapter, timeout: Duration) -> Result<Self> {
        let peripheral = find_by_name(adapter, v2::BLE_NAME, timeout).await?;
        Self::connect_peripheral(peripheral).await
    }

    /// Connect to an already discovered peripheral and complete initialization.
    pub async fn connect_peripheral(peripheral: Peripheral) -> Result<Self> {
        if !peripheral.is_connected().await? {
            peripheral.connect().await?;
        }
        peripheral.discover_services().await?;

        let strength_char = require_characteristic(&peripheral, CHAR_STRENGTH).await?;
        let wave_a_char = require_characteristic(&peripheral, CHAR_WAVE_A).await?;
        let wave_b_char = require_characteristic(&peripheral, CHAR_WAVE_B).await?;
        let battery_char = require_characteristic(&peripheral, CHAR_BATTERY).await.ok();

        // PWM_AB2 supports notifications, used to receive strength changes caused by physical input such as the dial
        let _ = peripheral.subscribe(&strength_char).await;
        if let Some(battery) = &battery_char {
            let _ = peripheral.subscribe(battery).await;
        }

        let write_type = preferred_write_type(&wave_a_char);
        Ok(Coyote2 {
            peripheral,
            strength_char,
            wave_a_char,
            wave_b_char,
            battery_char,
            write_type,
        })
    }

    /// Underlying peripheral handle.
    pub fn peripheral(&self) -> &Peripheral {
        &self.peripheral
    }

    /// Write the actual strength of both channels (0..=2047); it is held after writing.
    ///
    /// Every +1 of the value displayed in the official app corresponds to +7 of
    /// actual strength; use [`v2::STRENGTH_PER_APP_LEVEL`] to convert.
    pub async fn set_strength(&self, a: u16, b: u16) -> Result<()> {
        let data = encode_strength(a, b)?;
        self.peripheral
            .write(&self.strength_char, &data, WriteType::WithResponse)
            .await?;
        Ok(())
    }

    /// Read the current actual strength `(A, B)` of both channels.
    pub async fn strength(&self) -> Result<(u16, u16)> {
        let data = self.peripheral.read(&self.strength_char).await?;
        let bytes: [u8; 3] = data
            .get(..3)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| Error::Parse(format!("PWM_AB2 data too short: {data:02X?}")))?;
        Ok(decode_strength(bytes))
    }

    /// Write the channel A waveform parameters (valid for 0.1s only).
    pub async fn set_waveform_a(&self, wave: &Waveform) -> Result<()> {
        self.peripheral
            .write(&self.wave_a_char, &wave.encode(), self.write_type)
            .await?;
        Ok(())
    }

    /// Write the channel B waveform parameters (valid for 0.1s only).
    pub async fn set_waveform_b(&self, wave: &Waveform) -> Result<()> {
        self.peripheral
            .write(&self.wave_b_char, &wave.encode(), self.write_type)
            .await?;
        Ok(())
    }

    /// Zero out the strength of both channels.
    pub async fn stop(&self) -> Result<()> {
        self.set_strength(0, 0).await
    }

    /// Read the current battery level (0-100).
    pub async fn battery_level(&self) -> Result<u8> {
        let battery = self
            .battery_char
            .as_ref()
            .ok_or(Error::CharacteristicNotFound(CHAR_BATTERY))?;
        let data = self.peripheral.read(battery).await?;
        data.first()
            .copied()
            .ok_or_else(|| Error::Parse("battery characteristic returned no data".into()))
    }

    /// Device event stream: strength changes and battery notifications.
    pub async fn events(&self) -> Result<BoxStream<'static, Coyote2Event>> {
        let stream = self.peripheral.notifications().await?;
        Ok(stream
            .filter_map(|n| async move {
                if n.uuid == CHAR_STRENGTH {
                    let bytes: [u8; 3] = n.value.get(..3)?.try_into().ok()?;
                    let (a, b) = decode_strength(bytes);
                    Some(Coyote2Event::Strength { a, b })
                } else if n.uuid == CHAR_BATTERY {
                    n.value.first().copied().map(Coyote2Event::Battery)
                } else {
                    None
                }
            })
            .boxed())
    }

    /// Disconnect (this does not zero out the strength; call [`stop`] first if needed).
    ///
    /// [`stop`]: Coyote2::stop
    pub async fn disconnect(&self) -> Result<()> {
        self.peripheral.disconnect().await?;
        Ok(())
    }
}
