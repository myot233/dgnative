/// Error type of dgnative.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A parameter is outside the range allowed by the protocol.
    #[error("{field} = {value} is out of protocol range {min}..={max}")]
    OutOfRange {
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },

    /// A message returned by the device could not be parsed.
    #[error("cannot parse reply message: {0}")]
    Parse(String),

    /// No Bluetooth adapter was found.
    #[cfg(feature = "ble")]
    #[error("no Bluetooth adapter found")]
    NoAdapter,

    /// The target device was not discovered before the timeout elapsed.
    #[cfg(feature = "ble")]
    #[error("device {0:?} not found by scan")]
    DeviceNotFound(&'static str),

    /// A GATT characteristic required by the protocol was missing after connecting.
    #[cfg(feature = "ble")]
    #[error("device is missing characteristic {0}")]
    CharacteristicNotFound(uuid::Uuid),

    /// Underlying btleplug error.
    #[cfg(feature = "ble")]
    #[error(transparent)]
    Ble(#[from] btleplug::Error),
}
