//! # dgnative
//!
//! Rust implementation of the DG-LAB Coyote pulse host (Coyote V2 / V3)
//! Bluetooth protocol.
//!
//! The protocol follows the official open-source documentation:
//! <https://github.com/DG-LAB-OPENSOURCE/DG-LAB-OPENSOURCE>
//!
//! Two layers:
//!
//! - [`protocol`]: pure protocol codec, no IO, usable on any platform;
//! - [`ble`] (`ble` feature, on by default): cross-platform BLE transport
//!   built on [btleplug], providing the [`ble::Coyote3`] / [`ble::Coyote2`]
//!   device clients.
//!
//! [btleplug]: https://github.com/deviceplug/btleplug
//!
//! ## V3 quick start
//!
//! ```no_run
//! use dgnative::ble::Coyote3;
//! use dgnative::protocol::v3::{B0, Bf, Pulse, StrengthAction};
//! use std::time::Duration;
//!
//! # async fn demo() -> Result<(), dgnative::Error> {
//! // Scan for and connect to the pulse host named 47L121000
//! let coyote = Coyote3::scan_and_connect(Duration::from_secs(10)).await?;
//!
//! // ⚠️ After every reconnect you must first write a BF command to set the soft strength limit
//! coyote.set_config(&Bf::with_limits(50, 50)).await?;
//!
//! // Write one B0 every 100ms: set channel A strength to 10 and output a 100Hz waveform
//! let cmd = B0 {
//!     sequence: 1,
//!     action_a: StrengthAction::Set(10),
//!     pulses_a: [Pulse::new(10, 50)?; 4],
//!     ..B0::default()
//! };
//! coyote.send(&cmd).await?;
//! # Ok(())
//! # }
//! ```

pub mod protocol;

#[cfg(feature = "ble")]
pub mod ble;

mod error;
pub use error::Error;

/// The unified Result type for this crate.
pub type Result<T, E = Error> = core::result::Result<T, E>;
