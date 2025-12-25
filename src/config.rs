use alloc::vec::Vec;
use axvmconfig::EmulatedDeviceConfig;

/// The vector of DeviceConfig
pub struct AxVmDeviceConfig {
    /// The vector of EmulatedDeviceConfig
    pub emu_configs: Vec<EmulatedDeviceConfig>,
    /// The vector of VirtioBlkMmioDeviceConfig
    pub virtio_blk_configs: Vec<axvmconfig::VirtioBlkMmioDeviceConfig>,
}

/// The implemention for AxVmDeviceConfig
impl AxVmDeviceConfig {
    /// The new function for AxVmDeviceConfig
    pub fn new(
        emu_configs: Vec<EmulatedDeviceConfig>,
        virtio_blk_configs: Vec<axvmconfig::VirtioBlkMmioDeviceConfig>,
    ) -> Self {
        Self {
            emu_configs,
            virtio_blk_configs,
        }
    }
}
