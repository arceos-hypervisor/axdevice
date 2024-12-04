use crate::AxVmDeviceConfig;

use alloc::sync::Arc;
use alloc::vec::Vec;

use axaddrspace::{device::{AccessWidth, DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange}, GuestPhysAddr, GuestPhysAddrRange};
use axdevice_base::{BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps, EmuDeviceType, EmulatedDeviceConfig, VCpuInfo};
use axerrno::AxResult;

pub struct AxEmuDevices<R: DeviceAddrRange, U: VCpuInfo> {
    emu_devices: Vec<Arc<dyn BaseDeviceOps<R, U>>>,
}

impl<R: DeviceAddrRange, U: VCpuInfo> AxEmuDevices<R, U> {
    pub fn new() -> Self {
        Self {
            emu_devices: Vec::new(),
        }
    }

    pub fn find_dev(&self, addr: R::Addr) -> Option<Arc<dyn BaseDeviceOps<R, U>>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(addr))
            .cloned()
    }
}

type AxEmuMmioDevices<U> = AxEmuDevices<GuestPhysAddrRange, U>;
type AxEmuSysRegDevices<U> = AxEmuDevices<SysRegAddrRange, U>;
type AxEmuPortDevices<U> = AxEmuDevices<PortRange, U>;

/// represent A vm own devices
pub struct AxVmDevices<U: VCpuInfo> {
    /// emu devices
    emu_mmio_devices: AxEmuMmioDevices<U>,
    emu_sysreg_devices: AxEmuSysRegDevices<U>,
    emu_port_devices: AxEmuPortDevices<U>,
    // TODO passthrough devices or other type devices ...
}

/// The implemention for AxVmDevices
impl<U: VCpuInfo> AxVmDevices<U> {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> Self {
        let mut this = Self {
            emu_mmio_devices: AxEmuMmioDevices::new(),
            emu_sysreg_devices: AxEmuSysRegDevices::new(),
            emu_port_devices: AxEmuPortDevices::new(),
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

    /// Find specific MMIO device by ipa
    pub fn find_mmio_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn BaseMmioDeviceOps<U>>> {
        self.emu_mmio_devices.find_dev(ipa)
    }

    /// Find specific system register device by ipa
    pub fn find_sysreg_dev(&self, sysreg_addr: SysRegAddr) -> Option<Arc<dyn BaseSysRegDeviceOps<U>>> {
        self.emu_sysreg_devices.find_dev(sysreg_addr)
    }

    /// Find specific port device by port number
    pub fn find_port_dev(&self, port: Port) -> Option<Arc<dyn BasePortDeviceOps<U>>> {
        self.emu_port_devices.find_dev(port)
    }

    /// Handle the MMIO read by GuestPhysAddr and data width, return the value of the guest want to read
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
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
    pub fn handle_mmio_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
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

    /// Handle the system register read by SysRegAddr and data width, return the value of the guest want to read
    pub fn handle_sysreg_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_sysreg_dev(addr) {
            info!(
                "emu: {:?} handler read sysreg {:#x}",
                emu_dev.address_range(),
                addr.0
            );
            return emu_dev.handle_read(addr, width);
        }
        panic!("emu_handle: no emul handler for sysreg read {:#x}", addr.0);
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_sysreg_write(&self, addr: SysRegAddr, width: AccessWidth, val: usize) {
        if let Some(emu_dev) = self.find_sysreg_dev(addr) {
            info!(
                "emu: {:?} handler write sysreg {:#x}",
                emu_dev.address_range(),
                addr.0
            );
            emu_dev.handle_write(addr, width, val);
            return;
        }
        panic!("emu_handler: no emul handler for sysreg write {:#x}", addr.0);
    }

    /// Handle the port read by port number and data width, return the value of the guest want to read
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_port_dev(port) {
            info!("emu: {:?} handler read port {:#x}", emu_dev.address_range(), port.0);
            return emu_dev.handle_read(port, width);
        }
        panic!("emu_handle: no emul handler for port read {:#x}", port.0);
    }

    /// Handle the port write by port number, data width and the value need to write, call specific device to write the value
    pub fn handle_port_write(&self, port: Port, width: AccessWidth, val: usize) {
        if let Some(emu_dev) = self.find_port_dev(port) {
            info!("emu: {:?} handler write port {:#x}", emu_dev.address_range(), port.0);
            emu_dev.handle_write(port, width, val);
            return;
        }
        panic!("emu_handler: no emul handler for port write {:#x}", port.0);
    }
}
