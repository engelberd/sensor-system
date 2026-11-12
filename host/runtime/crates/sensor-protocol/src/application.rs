use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ServiceId {
    Identity = 1,
    Measurement = 2,
    Time = 3,
    Health = 4,
    Configuration = 5,
    Lifecycle = 6,
    Diagnostics = 7,
    Commissioning = 8,
    Telemetry = 9,
}

impl ServiceId {
    pub const fn capability_bit(self) -> u32 {
        1u32 << ((self as u8) - 1)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ResponseStatus {
    Ok = 0,
    MalformedRequest = 1,
    UnsupportedService = 2,
    UnsupportedMessage = 3,
    WrongMode = 4,
    Busy = 5,
    Conflict = 6,
    InvalidState = 7,
    InternalError = 8,
}

impl TryFrom<u8> for ResponseStatus {
    type Error = ApplicationContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::MalformedRequest),
            2 => Ok(Self::UnsupportedService),
            3 => Ok(Self::UnsupportedMessage),
            4 => Ok(Self::WrongMode),
            5 => Ok(Self::Busy),
            6 => Ok(Self::Conflict),
            7 => Ok(Self::InvalidState),
            8 => Ok(Self::InternalError),
            _ => Err(ApplicationContractError::UnknownResponseStatus(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationContractError {
    UnknownResponseStatus(u8),
}

impl fmt::Display for ApplicationContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownResponseStatus(status) => {
                write!(formatter, "unknown application response status {status}")
            }
        }
    }
}

impl std::error::Error for ApplicationContractError {}
