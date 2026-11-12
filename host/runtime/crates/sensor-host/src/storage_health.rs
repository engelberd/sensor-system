use core::fmt;
use std::path::{Path, PathBuf};

use nix::sys::statvfs::{statvfs, FsFlags};

use crate::host_health::SharedStorageFault;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageHealthPolicy {
    pub minimum_available_bytes: u64,
}

impl StorageHealthPolicy {
    pub fn validate(self) -> Result<Self, StorageHealthProbeError> {
        if self.minimum_available_bytes == 0 {
            Err(StorageHealthProbeError::ZeroReserve)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthyStorage {
    pub available_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageHealthObservation {
    Healthy(HealthyStorage),
    Fault(SharedStorageFault),
}

/// Read-only platform adapter for the filesystem shared by Capture writers.
/// It never creates a probe file in the recording path and therefore cannot
/// be mistaken for proof that a future durability barrier will succeed.
pub struct StorageHealthProbe {
    root: PathBuf,
    policy: StorageHealthPolicy,
}

impl StorageHealthProbe {
    pub fn new(
        root: impl Into<PathBuf>,
        policy: StorageHealthPolicy,
    ) -> Result<Self, StorageHealthProbeError> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(StorageHealthProbeError::EmptyRoot);
        }
        Ok(Self {
            root,
            policy: policy.validate()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn minimum_available_bytes(&self) -> u64 {
        self.policy.minimum_available_bytes
    }

    pub fn observe(&self) -> Result<StorageHealthObservation, StorageHealthProbeError> {
        let statistics = statvfs(&self.root).map_err(StorageHealthProbeError::Statvfs)?;
        if statistics.flags().contains(FsFlags::ST_RDONLY) {
            return Ok(StorageHealthObservation::Fault(
                SharedStorageFault::ReadOnly,
            ));
        }
        let available_bytes = u64::from(statistics.blocks_available())
            .checked_mul(statistics.fragment_size())
            .ok_or(StorageHealthProbeError::AvailableBytesOverflow)?;
        if available_bytes < self.policy.minimum_available_bytes {
            Ok(StorageHealthObservation::Fault(
                SharedStorageFault::ReserveBreached {
                    available_bytes,
                    required_bytes: self.policy.minimum_available_bytes,
                },
            ))
        } else {
            Ok(StorageHealthObservation::Healthy(HealthyStorage {
                available_bytes,
            }))
        }
    }
}

#[derive(Debug)]
pub enum StorageHealthProbeError {
    EmptyRoot,
    ZeroReserve,
    Statvfs(nix::Error),
    AvailableBytesOverflow,
}

impl fmt::Display for StorageHealthProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "storage health probe error: {self:?}")
    }
}

impl std::error::Error for StorageHealthProbeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_policy_before_platform_io() {
        assert!(matches!(
            StorageHealthProbe::new(
                "capture",
                StorageHealthPolicy {
                    minimum_available_bytes: 0
                }
            ),
            Err(StorageHealthProbeError::ZeroReserve)
        ));
        assert!(matches!(
            StorageHealthProbe::new(
                "",
                StorageHealthPolicy {
                    minimum_available_bytes: 1
                }
            ),
            Err(StorageHealthProbeError::EmptyRoot)
        ));
    }

    #[test]
    fn observes_the_real_filesystem_without_writing_to_it() {
        let probe = StorageHealthProbe::new(
            std::env::temp_dir(),
            StorageHealthPolicy {
                minimum_available_bytes: 1,
            },
        )
        .unwrap();
        assert!(matches!(
            probe.observe().unwrap(),
            StorageHealthObservation::Healthy(HealthyStorage {
                available_bytes: 1..
            })
        ));
    }

    #[test]
    fn reports_a_reserve_breach_as_a_typed_shared_fault() {
        let probe = StorageHealthProbe::new(
            std::env::temp_dir(),
            StorageHealthPolicy {
                minimum_available_bytes: u64::MAX,
            },
        )
        .unwrap();
        assert!(matches!(
            probe.observe().unwrap(),
            StorageHealthObservation::Fault(SharedStorageFault::ReserveBreached {
                required_bytes: u64::MAX,
                ..
            })
        ));
    }
}
