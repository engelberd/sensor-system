#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HardwareId([u8; 8]);

impl HardwareId {
    pub const fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 8] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BootEpoch(u64);

impl BootEpoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SampleSequence(u64);

impl SampleSequence {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn checked_add(self, count: u64) -> Option<Self> {
        match self.0.checked_add(count) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SampleIdentity {
    pub hardware_id: HardwareId,
    pub boot_epoch: BootEpoch,
    pub sequence: SampleSequence,
}

impl SampleIdentity {
    pub const fn new(
        hardware_id: HardwareId,
        boot_epoch: BootEpoch,
        sequence: SampleSequence,
    ) -> Self {
        Self {
            hardware_id,
            boot_epoch,
            sequence,
        }
    }
}
