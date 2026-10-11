use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersion {
    pub framing: u8,
    pub application: u8,
}

impl ProtocolVersion {
    pub const fn new(framing: u8, application: u8) -> Self {
        Self {
            framing,
            application,
        }
    }

    pub const fn check_compatible(self, remote: Self) -> Result<(), ProtocolMismatch> {
        if self.framing == remote.framing && self.application == remote.application {
            Ok(())
        } else {
            Err(ProtocolMismatch {
                local: self,
                remote,
            })
        }
    }
}

pub const LOCAL_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(3, 5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolMismatch {
    pub local: ProtocolVersion,
    pub remote: ProtocolVersion,
}

impl fmt::Display for ProtocolMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "protocol mismatch: local framing/application {}/{}, remote {}/{}",
            self.local.framing,
            self.local.application,
            self.remote.framing,
            self.remote.application
        )
    }
}

impl std::error::Error for ProtocolMismatch {}

#[cfg(test)]
mod tests {
    use super::{ProtocolVersion, LOCAL_PROTOCOL_VERSION};

    #[test]
    fn exact_protocol_version_is_compatible() {
        assert!(LOCAL_PROTOCOL_VERSION
            .check_compatible(ProtocolVersion::new(3, 5))
            .is_ok());
    }

    #[test]
    fn framing_or_application_change_requires_explicit_support() {
        assert!(LOCAL_PROTOCOL_VERSION
            .check_compatible(ProtocolVersion::new(2, 5))
            .is_err());
        assert!(LOCAL_PROTOCOL_VERSION
            .check_compatible(ProtocolVersion::new(3, 4))
            .is_err());
    }
}
