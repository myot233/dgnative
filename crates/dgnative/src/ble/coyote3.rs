//! Coyote 3.0 BLE client.

use crate::protocol::v3::{
    self, B0, Bf, CHAR_BATTERY, CHAR_NOTIFY, CHAR_WRITE, Notification, parse_notification,
};
use crate::{Error, Result};
use btleplug::api::{Characteristic, Peripheral as _, WriteType};
use btleplug::platform::{Adapter, Peripheral};
use futures::StreamExt;
use futures::stream::BoxStream;
use std::time::Duration;

use super::{default_adapter, find_by_name, preferred_write_type, require_characteristic};

/// Default timeout for connecting and service discovery.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Event returned by [`Coyote3::events`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coyote3Event {
    /// Protocol message returned by characteristic 0x150B (B1 strength report, etc.).
    Message(Notification),
    /// Battery change notification (0-100).
    Battery(u8),
}

/// Coyote 3.0 (Bluetooth name `47L121000`) client.
///
/// The connect flow wraps the initialization the protocol requires: discover
/// services, subscribe to the 0x150B notifications.
/// ⚠️ Right after connecting, call [`set_config`] to write a BF command that
/// sets the soft limit, then call [`send`] every 100ms to write B0 commands
/// and keep the waveform output going.
///
/// [`set_config`]: Coyote3::set_config
/// [`send`]: Coyote3::send
pub struct Coyote3 {
    peripheral: Peripheral,
    write_char: Characteristic,
    battery_char: Option<Characteristic>,
    write_type: WriteType,
}

impl Coyote3 {
    /// Scan with the system's first Bluetooth adapter and connect to the first Coyote 3.0.
    pub async fn scan_and_connect(timeout: Duration) -> Result<Self> {
        let adapter = default_adapter().await?;
        Self::scan_and_connect_with(&adapter, timeout).await
    }

    /// Scan with the given adapter and connect to the first Coyote 3.0.
    pub async fn scan_and_connect_with(adapter: &Adapter, timeout: Duration) -> Result<Self> {
        let peripheral = find_by_name(adapter, v3::BLE_NAME, timeout).await?;
        Self::connect_peripheral(peripheral).await
    }

    /// Connect to an already discovered peripheral and complete protocol
    /// initialization, using [`DEFAULT_CONNECT_TIMEOUT`] as the connect timeout.
    pub async fn connect_peripheral(peripheral: Peripheral) -> Result<Self> {
        Self::connect_peripheral_with_timeout(peripheral, DEFAULT_CONNECT_TIMEOUT).await
    }

    /// Connect to an already discovered peripheral and complete protocol initialization.
    ///
    /// The timeout is necessary: a device that shows up in the advertisement
    /// scan but cannot actually be connected to (signal too weak, already held
    /// by another program) makes `connect()` hang indefinitely.
    pub async fn connect_peripheral_with_timeout(
        peripheral: Peripheral,
        timeout: Duration,
    ) -> Result<Self> {
        if !peripheral.is_connected().await? {
            peripheral.connect_with_timeout(timeout).await?;
        }
        peripheral.discover_services_with_timeout(timeout).await?;

        let write_char = require_characteristic(&peripheral, CHAR_WRITE).await?;
        let notify_char = require_characteristic(&peripheral, CHAR_NOTIFY).await?;
        let battery_char = require_characteristic(&peripheral, CHAR_BATTERY).await.ok();

        peripheral.subscribe(&notify_char).await?;
        if let Some(battery) = &battery_char {
            // The battery characteristic supports notifications; a failed subscribe does not affect core functionality
            let _ = peripheral.subscribe(battery).await;
        }

        let write_type = preferred_write_type(&write_char);
        Ok(Coyote3 {
            peripheral,
            write_char,
            battery_char,
            write_type,
        })
    }

    /// Underlying peripheral handle.
    pub fn peripheral(&self) -> &Peripheral {
        &self.peripheral
    }

    /// Device id, matching [`Discovered::id`](crate::ble::Discovered::id): the
    /// MAC address on Linux / Windows, the CoreBluetooth UUID on macOS.
    pub fn id(&self) -> String {
        self.peripheral.id().to_string()
    }

    /// Write one B0 command (should be called every 100ms to keep the output alive).
    pub async fn send(&self, cmd: &B0) -> Result<()> {
        self.write(&cmd.encode()).await
    }

    /// Write a BF command: soft strength limit + balance parameters.
    ///
    /// BF has no reply and persists across power cycles, but **it must be
    /// rewritten after every reconnect**.
    pub async fn set_config(&self, config: &Bf) -> Result<()> {
        self.write(&config.encode()).await
    }

    /// Immediately zero out both channel strengths (send one B0 with no
    /// waveform and Set(0) on both channels).
    pub async fn stop(&self) -> Result<()> {
        use crate::protocol::v3::StrengthAction;
        self.send(&B0 {
            sequence: 0,
            action_a: StrengthAction::Set(0),
            action_b: StrengthAction::Set(0),
            ..B0::default()
        })
        .await
    }

    async fn write(&self, data: &[u8]) -> Result<()> {
        self.peripheral
            .write(&self.write_char, data, self.write_type)
            .await?;
        Ok(())
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

    /// Device event stream: B1 strength reports and battery notifications.
    ///
    /// Unparseable 0x150B data is surfaced as [`Notification::Unknown`].
    pub async fn events(&self) -> Result<BoxStream<'static, Coyote3Event>> {
        let stream = self.peripheral.notifications().await?;
        Ok(stream
            .filter_map(|n| async move {
                if n.uuid == CHAR_NOTIFY {
                    match parse_notification(&n.value) {
                        Ok(msg) => Some(Coyote3Event::Message(msg)),
                        Err(_) => Some(Coyote3Event::Message(Notification::Unknown(n.value))),
                    }
                } else if n.uuid == CHAR_BATTERY {
                    n.value.first().copied().map(Coyote3Event::Battery)
                } else {
                    None
                }
            })
            .boxed())
    }

    /// Disconnect (this does not zero out the strength; call [`stop`] first if needed).
    ///
    /// [`stop`]: Coyote3::stop
    pub async fn disconnect(&self) -> Result<()> {
        self.peripheral.disconnect().await?;
        Ok(())
    }
}
