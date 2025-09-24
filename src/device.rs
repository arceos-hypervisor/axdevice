use crate::AxVmDeviceConfig;

use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;

use axaddrspace::GuestPhysAddr;
use axdevice_base::BaseDeviceOps;
use axerrno::AxResult;
use axvirtpci::{PciHost, PioOps};
use axvmconfig::EmulatedDeviceConfig;

/// represent A vm own devices
pub struct AxVmDevices {
    /// emu devices
    emu_devices: Vec<Arc<dyn BaseDeviceOps>>,
    /// A Dummy pci host only for guest VM.
    pci_host: Option<Arc<Mutex<PciHost>>>,
    // TODO passthrough devices or other type devices ...
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig, is_host: bool) -> Self {
        let mut this = Self {
            emu_devices: Vec::new(),
            pci_host: if !is_host {
                Some(Arc::new(Mutex::new(PciHost::new())))
            } else {
                None
            },
        };

        Self::init(&mut this, &config.emu_configs);
        this
    }

    /// According the emu_configs to init every  specific device
    fn init(_this: &mut Self, _emu_configs: &Vec<EmulatedDeviceConfig>) {
        /*
        for config in emu_configs {
            let dev = match EmuDeviceType::from_usize(config.emu_type) {
                // todo call specific initialization function of devcise
                EmuDeviceType::EmuDeviceTConsole => ,
                EmuDeviceType::EmuDeviceTGicdV2 => ,
                EmuDeviceType::EmuDeviceTGPPT => ,
                EmuDeviceType::EmuDeviceTVirtioBlk => ,
                EmuDeviceType::EmuDeviceTVirtioNet => ,
                EmuDeviceType::EmuDeviceTVirtioConsole => ,
                EmuDeviceType::EmuDeviceTIOMMU => ,
                EmuDeviceType::EmuDeviceTICCSRE => ,
                EmuDeviceType::EmuDeviceTSGIR => ,
                EmuDeviceType::EmuDeviceTGICR => ,
                EmuDeviceType::EmuDeviceTMeta => ,
                _ => panic!("emu type: {} is still not supported", config.emu_type),
            };
            if let Ok(emu_dev) = dev {
                this.emu_devices.push(emu_dev)
            }
        }
        */
    }

    /// Find specific device by ipa
    pub fn find_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn BaseDeviceOps>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(ipa))
            .cloned()
    }

    /// Handle the MMIO read by GuestPhysAddr and data width, return the value of the guest want to read
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: usize) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_dev(addr) {
            info!(
                "emu: {:?} handler read ipa {:#x}",
                emu_dev.address_range(),
                addr
            );
            return emu_dev.handle_read(addr, width);
        }
        panic!("emu_handle: no emul handler for data abort ipa {:#x}", addr);
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_mmio_write(&self, addr: GuestPhysAddr, width: usize, val: usize) {
        if let Some(emu_dev) = self.find_dev(addr) {
            info!(
                "emu: {:?} handler write ipa {:#x}",
                emu_dev.address_range(),
                addr
            );
            emu_dev.handle_write(addr, width, val);
            return;
        }
        panic!(
            "emu_handler: no emul handler for data abort ipa {:#x}",
            addr
        );
    }

    /// Handle the PIO read by port and access_size, return the value of the guest want to read.
    /// TODO: merge it with new version of [`axdevice_base::BasePortDeviceOps`](https://github.com/arceos-hypervisor/axdevice_base/blob/35da6c7b5a29d01410df6ca31300bbfc50eb4603/src/lib.rs#L80C11-L80C29)
    pub fn handle_pio_read(&self, port: u16, access_size: u8) -> AxResult<u32> {
        match &self.pci_host {
            Some(pci_host) => {
                let mut pci_host = pci_host.lock();
                if pci_host.port_range().contains(&port) {
                    return pci_host.read(port, access_size);
                } else {
                    return Err(axerrno::AxError::InvalidInput);
                }
            }
            None => Err(axerrno::AxError::InvalidInput),
        }
    }

    /// Handle the PIO write by port, access_size and the value need to write, call specific device to write the value.
    ///
    /// Currently, only the pci host device support PIO operation.
    // TODO: merge it with new version of [`axdevice_base::BasePortDeviceOps`](https://github.com/arceos-hypervisor/axdevice_base/blob/35da6c7b5a29d01410df6ca31300bbfc50eb4603/src/lib.rs#L80C11-L80C29)
    pub fn handle_pio_write(&self, port: u16, access_size: u8, value: u32) -> AxResult {
        match &self.pci_host {
            Some(pci_host) => {
                let mut pci_host = pci_host.lock();
                if pci_host.port_range().contains(&port) {
                    return pci_host.write(port, access_size, value);
                } else {
                    return Err(axerrno::AxError::InvalidInput);
                }
            }
            None => Err(axerrno::AxError::InvalidInput),
        }
    }
}
