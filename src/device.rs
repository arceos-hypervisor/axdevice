use core::panic;

use crate::AxVmDeviceConfig;

use alloc::sync::Arc;
use alloc::vec::Vec;

use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::{
    BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps, DeviceRWContext,
    EmuDeviceType,
};
use axerrno::AxResult;
use axvmconfig::EmulatedDeviceConfig;

pub struct AxEmuDevices<R: DeviceAddrRange> {
    emu_devices: Vec<Arc<dyn BaseDeviceOps<R>>>,
}

impl<R: DeviceAddrRange> AxEmuDevices<R> {
    pub fn new() -> Self {
        Self {
            emu_devices: Vec::new(),
        }
    }

    pub fn find_dev(&self, addr: R::Addr) -> Option<Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(addr))
            .cloned()
    }
}

type AxEmuMmioDevices = AxEmuDevices<GuestPhysAddrRange>;
type AxEmuSysRegDevices = AxEmuDevices<SysRegAddrRange>;
type AxEmuPortDevices = AxEmuDevices<PortRange>;

/// represent A vm own devices
pub struct AxVmDevices {
    /// emu devices
    emu_mmio_devices: AxEmuMmioDevices,
    emu_sys_reg_devices: AxEmuSysRegDevices,
    emu_port_devices: AxEmuPortDevices,
    // TODO passthrough devices or other type devices ...
}

#[inline]
fn log_device_io(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    addr_range: impl core::fmt::LowerHex,
    read: bool,
    width: AccessWidth,
) {
    let rw = if read { "read" } else { "write" };
    info!(
        "emu_device {}: {} {:#x} in range {:#x} with width {:?}",
        rw, addr_type, addr, addr_range, width
    )
}

#[inline]
fn panic_device_not_found(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    read: bool,
    width: AccessWidth,
) -> ! {
    let rw = if read { "read" } else { "write" };
    error!(
        "emu_device {} failed: device not found for {} {:#x} with width {:?}",
        rw, addr_type, addr, width
    );
    panic!("emu_device not found");
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> Self {
        let mut this = Self {
            emu_mmio_devices: AxEmuMmioDevices::new(),
            emu_sys_reg_devices: AxEmuSysRegDevices::new(),
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
    pub fn find_mmio_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.find_dev(ipa)
    }

    /// Find specific system register device by ipa
    pub fn find_sys_reg_dev(
        &self,
        sys_reg_addr: SysRegAddr,
    ) -> Option<Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.find_dev(sys_reg_addr)
    }

    /// Find specific port device by port number
    pub fn find_port_dev(&self, port: Port) -> Option<Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.find_dev(port)
    }

    /// Handle the MMIO read by GuestPhysAddr and data width, return the value of the guest want to read
    pub fn handle_mmio_read(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
            log_device_io("mmio", addr, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(addr, width, context);
        }
        panic_device_not_found("mmio", addr, true, width);
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        context: DeviceRWContext,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
            log_device_io("mmio", addr, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(addr, width, val, context);
        }
        panic_device_not_found("mmio", addr, false, width);
    }

    /// Handle the system register read by SysRegAddr and data width, return the value of the guest want to read
    pub fn handle_sys_reg_read(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_sys_reg_dev(addr) {
            log_device_io("sys_reg", addr.0, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(addr, width, context);
        }
        panic_device_not_found("sys_reg", addr, true, width);
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_sys_reg_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
        context: DeviceRWContext,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_sys_reg_dev(addr) {
            log_device_io("sys_reg", addr.0, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(addr, width, val, context);
        }
        panic_device_not_found("sys_reg", addr, false, width);
    }

    /// Handle the port read by port number and data width, return the value of the guest want to read
    pub fn handle_port_read(
        &self,
        port: Port,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_port_dev(port) {
            log_device_io("port", port.0, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(port, width, context);
        }
        panic_device_not_found("port", port, true, width);
    }

    /// Handle the port write by port number, data width and the value need to write, call specific device to write the value
    pub fn handle_port_write(
        &self,
        port: Port,
        width: AccessWidth,
        val: usize,
        context: DeviceRWContext,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_port_dev(port) {
            log_device_io("port", port.0, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(port, width, val, context);
        }
        panic_device_not_found("port", port, false, width);
    }
}
