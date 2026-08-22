//! Cross-platform BLE transport layer built on
//! [btleplug](https://github.com/deviceplug/btleplug) (`ble` feature).
//!
//! - [`Coyote3`]: Coyote 3.0 client;
//! - [`Coyote2`]: Coyote 2.0 client.

mod coyote2;
mod coyote3;

pub use coyote2::{Coyote2, Coyote2Event};
pub use coyote3::{Coyote3, Coyote3Event};

use crate::{Error, Result};
use btleplug::api::{
    Central, CentralEvent, CharPropFlags, Characteristic, Manager as _, Peripheral as _,
    ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use std::time::Duration;

/// Take the first Bluetooth adapter of the system.
pub async fn default_adapter() -> Result<Adapter> {
    Manager::new()
        .await?
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or(Error::NoAdapter)
}

async fn matches_name(peripheral: &Peripheral, name: &str) -> bool {
    match peripheral.properties().await {
        Ok(Some(props)) => props.local_name.is_some_and(|n| n.trim().starts_with(name)),
        _ => false,
    }
}

/// A recognized DG-LAB device model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Pulse host 3.0 (47L121000).
    Coyote3,
    /// Pulse host 2.0 (D-LAB ESTIM01).
    Coyote2,
    /// Wireless sensor (47L120100).
    WirelessSensor,
}

impl DeviceKind {
    /// Identify the model from the advertised name.
    pub fn from_local_name(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.starts_with(crate::protocol::v3::BLE_NAME) {
            Some(DeviceKind::Coyote3)
        } else if name.starts_with(crate::protocol::v3::BLE_NAME_WIRELESS_SENSOR) {
            Some(DeviceKind::WirelessSensor)
        } else if name.starts_with(crate::protocol::v2::BLE_NAME) {
            Some(DeviceKind::Coyote2)
        } else {
            None
        }
    }

    /// Display name of the model.
    pub fn label(&self) -> &'static str {
        match self {
            DeviceKind::Coyote3 => "Pulse host 3.0",
            DeviceKind::Coyote2 => "Pulse host 2.0",
            DeviceKind::WirelessSensor => "Wireless sensor",
        }
    }
}

/// One DG-LAB device found by a scan.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// Device model.
    pub kind: DeviceKind,
    /// Advertised name.
    pub local_name: String,
    /// Device id. On Linux / Windows this is the MAC address, on macOS it is
    /// the UUID assigned by CoreBluetooth (that platform does not expose the MAC).
    pub id: String,
    /// Signal strength.
    pub rssi: Option<i16>,
    /// Underlying peripheral handle, which can be passed straight to
    /// `Coyote3::connect_peripheral`.
    pub peripheral: Peripheral,
}

/// Scan for `timeout` and return every DG-LAB device found during that window
/// (sorted by descending signal strength).
pub async fn scan(adapter: &Adapter, timeout: Duration) -> Result<Vec<Discovered>> {
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(timeout).await;
    let peripherals = adapter.peripherals().await?;
    let _ = adapter.stop_scan().await;

    let mut found = Vec::new();
    for peripheral in peripherals {
        let Ok(Some(props)) = peripheral.properties().await else {
            continue;
        };
        let Some(local_name) = props.local_name else {
            continue;
        };
        let Some(kind) = DeviceKind::from_local_name(&local_name) else {
            continue;
        };
        found.push(Discovered {
            kind,
            local_name,
            id: peripheral.id().to_string(),
            rssi: props.rssi,
            peripheral,
        });
    }
    found.sort_by_key(|d| std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)));
    Ok(found)
}

/// Scan within `timeout` for a device whose advertised name starts with `name`.
pub async fn find_by_name(
    adapter: &Adapter,
    name: &'static str,
    timeout: Duration,
) -> Result<Peripheral> {
    let mut events = adapter.events().await?;
    adapter.start_scan(ScanFilter::default()).await?;

    let found = tokio::time::timeout(timeout, async {
        // The system may already have the target device cached
        for p in adapter.peripherals().await? {
            if matches_name(&p, name).await {
                return Ok(p);
            }
        }
        while let Some(event) = events.next().await {
            if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) = event {
                let p = adapter.peripheral(&id).await?;
                if matches_name(&p, name).await {
                    return Ok(p);
                }
            }
        }
        Err(Error::DeviceNotFound(name))
    })
    .await;

    let _ = adapter.stop_scan().await;
    found.map_err(|_elapsed| Error::DeviceNotFound(name))?
}

/// Take a characteristic by UUID after connecting and discovering services.
async fn require_characteristic(
    peripheral: &Peripheral,
    uuid: uuid::Uuid,
) -> Result<Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == uuid)
        .ok_or(Error::CharacteristicNotFound(uuid))
}

/// Prefer write-without-response when the characteristic supports it, which
/// lowers latency at the 100ms write cadence.
fn preferred_write_type(characteristic: &Characteristic) -> WriteType {
    if characteristic
        .properties
        .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
    {
        WriteType::WithoutResponse
    } else {
        WriteType::WithResponse
    }
}
