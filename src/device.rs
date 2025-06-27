use crate::AxVmDeviceConfig;

use alloc::vec::Vec;
use alloc::{format, sync::Arc};

use axaddrspace::device::AccessWidth;
use axaddrspace::GuestPhysAddr;
use axdevice_base::{BaseMmioDeviceOps, EmuDeviceType};
use axerrno::AxResult;
use axvirtio_blk::VirtioMmioDevice;
use axvmconfig::EmulatedDeviceConfig;

/// represent A vm own devices
pub struct AxVmDevices {
    /// emu devices
    emu_devices: Vec<Arc<dyn BaseMmioDeviceOps>>,
    // TODO passthrough devices or other type devices ...
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> Self {
        let mut this = Self {
            emu_devices: Vec::new(),
        };

        Self::init(&mut this, &config.emu_configs);
        this
    }

    /// According the emu_configs to init every  specific device
    fn init(this: &mut Self, emu_configs: &Vec<EmulatedDeviceConfig>) {
        for config in emu_configs {
            let dev = match EmuDeviceType::from_usize(config.emu_type) {
                // todo call specific initialization function of devcise
                // EmuDeviceType::EmuDeviceTConsole => ,
                // EmuDeviceType::EmuDeviceTGicdV2 => ,
                // EmuDeviceType::EmuDeviceTGPPT => ,
                EmuDeviceType::EmuDeviceTVirtioBlk => {
                    // Use the first non-zero index from cfg_list as device_index
                    let device_index = config.cfg_list.iter().position(|&x| x != 0).unwrap_or(0);
                    Ok(Arc::new(VirtioMmioDevice::new(config.base_gpa ,device_index).unwrap()))
                }
                // EmuDeviceType::EmuDeviceTVirtioNet => ,
                // EmuDeviceType::EmuDeviceTVirtioConsole => ,
                // EmuDeviceType::EmuDeviceTIOMMU => ,
                // EmuDeviceType::EmuDeviceTICCSRE => ,
                // EmuDeviceType::EmuDeviceTSGIR => ,
                // EmuDeviceType::EmuDeviceTGICR => ,
                // EmuDeviceType::EmuDeviceTMeta => ,
                _ => Err(format!(
                    "emu type: {} is still not supported",
                    config.emu_type
                )),
            };
            if let Ok(emu_dev) = dev {
                this.emu_devices.push(emu_dev)
            }
        }
    }

    /// Find specific device by ipa
    pub fn find_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn BaseMmioDeviceOps>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(ipa))
            .cloned()
    }

    /// Handle the MMIO read by GuestPhysAddr and data width, return the value of the guest want to read
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_dev(addr) {
            trace!(
                "emu: {:?} handler read ipa {:#x} width: {:?}",
                emu_dev.address_range(),
                addr,
                width
            );
            return emu_dev.handle_read(addr, width);
        }
        panic!("emu_handle: no emul handler for data abort ipa {:#x}", addr);
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_mmio_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) {
        if let Some(emu_dev) = self.find_dev(addr) {
            trace!(
                "emu: {:?} handler write ipa {:#x}",
                emu_dev.address_range(),
                addr
            );
            let _ = emu_dev.handle_write(addr, width, val);
            return;
        }
        panic!(
            "emu_handler: no emul handler for data abort ipa {:#x}",
            addr
        );
    }
}
