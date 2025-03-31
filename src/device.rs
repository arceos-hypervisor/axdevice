use core::panic;

use crate::AxVmDeviceConfig;

use alloc::format;
use alloc::vec::Vec;
use alloc::{boxed::Box, sync::Arc};
use arm_vgic::Vgic;
use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::{
    BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps, EmuDeviceType,
};
use axerrno::AxResult;
use axvmconfig::EmulatedDeviceConfig;

/// A set of emulated device types that can be accessed by a specific address range type.
pub struct AxEmuDevices<R: DeviceAddrRange> {
    emu_devices: Vec<Arc<dyn BaseDeviceOps<R>>>,
}

impl<R: DeviceAddrRange> AxEmuDevices<R> {
    /// Creates a new [`AxEmuDevices`] instance.
    pub fn new() -> Self {
        Self {
            emu_devices: Vec::new(),
        }
    }

    /// Adds a device to the set.
    pub fn add_dev(&mut self, dev: Arc<dyn BaseDeviceOps<R>>) {
        self.emu_devices.push(dev);
    }

    // pub fn remove_dev(&mut self, ...)
    //
    // `remove_dev` seems to need something like `downcast-rs` to make sense. As it's not likely to
    // be able to have a proper predicate to remove a device from the list without knowing the
    // concrete type of the device.

    /// Find a device by address.
    pub fn find_dev(&self, addr: R::Addr) -> Option<Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(addr))
            .cloned()
    }

    /// Iterates over the devices in the set.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices.iter()
    }

    /// Iterates over the devices in the set mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices.iter_mut()
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
    trace!(
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
    fn init(this: &mut Self, emu_configs: &Vec<EmulatedDeviceConfig>) {
        for config in emu_configs {
            let dev = match EmuDeviceType::from_usize(config.emu_type) {
                // todo call specific initialization function of devcise
                // EmuDeviceType::EmuDeviceTConsole => ,
                EmuDeviceType::EmuDeviceTInterruptController => Ok(Arc::new(Vgic::new())),
                // EmuDeviceType::EmuDeviceTGPPT => ,
                // EmuDeviceType::EmuDeviceTVirtioBlk => ,
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
                this.add_mmio_dev(emu_dev)
            }
        }
    }

    /// Add a MMIO device to the device list
    pub fn add_mmio_dev(&mut self, dev: Arc<dyn BaseMmioDeviceOps>) {
        self.emu_mmio_devices.add_dev(dev);
    }

    /// Add a system register device to the device list
    pub fn add_sys_reg_dev(&mut self, dev: Arc<dyn BaseSysRegDeviceOps>) {
        self.emu_sys_reg_devices.add_dev(dev);
    }

    /// Add a port device to the device list
    pub fn add_port_dev(&mut self, dev: Arc<dyn BasePortDeviceOps>) {
        self.emu_port_devices.add_dev(dev);
    }

    /// Iterates over the MMIO devices in the set.
    pub fn iter_mmio_dev(&self) -> impl Iterator<Item = &Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.iter()
    }

    /// Iterates over the system register devices in the set.
    pub fn iter_sys_reg_dev(&self) -> impl Iterator<Item = &Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.iter()
    }

    /// Iterates over the port devices in the set.
    pub fn iter_port_dev(&self) -> impl Iterator<Item = &Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.iter()
    }

    /// Iterates over the MMIO devices in the set.
    pub fn iter_mut_mmio_dev(&mut self) -> impl Iterator<Item = &mut Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.iter_mut()
    }

    /// Iterates over the system register devices in the set.
    pub fn iter_mut_sys_reg_dev(
        &mut self,
    ) -> impl Iterator<Item = &mut Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.iter_mut()
    }

    /// Iterates over the port devices in the set.
    pub fn iter_mut_port_dev(&mut self) -> impl Iterator<Item = &mut Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.iter_mut()
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
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
            log_device_io("mmio", addr, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(addr, width);
        }
        panic_device_not_found("mmio", addr, true, width);
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_mmio_dev(addr) {
            log_device_io("mmio", addr, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(addr, width, val);
        }
        panic_device_not_found("mmio", addr, false, width);
    }

    /// Handle the system register read by SysRegAddr and data width, return the value of the guest want to read
    pub fn handle_sys_reg_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_sys_reg_dev(addr) {
            log_device_io("sys_reg", addr.0, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(addr, width);
        }
        panic_device_not_found("sys_reg", addr, true, width);
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_sys_reg_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        if let Some(emu_dev) = self.find_sys_reg_dev(addr) {
            log_device_io("sys_reg", addr.0, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(addr, width, val);
        }
        panic_device_not_found("sys_reg", addr, false, width);
    }

    /// Handle the port read by port number and data width, return the value of the guest want to read
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        if let Some(emu_dev) = self.find_port_dev(port) {
            log_device_io("port", port.0, emu_dev.address_range(), true, width);

            return emu_dev.handle_read(port, width);
        }
        panic_device_not_found("port", port, true, width);
    }

    /// Handle the port write by port number, data width and the value need to write, call specific device to write the value
    pub fn handle_port_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        if let Some(emu_dev) = self.find_port_dev(port) {
            log_device_io("port", port.0, emu_dev.address_range(), false, width);

            return emu_dev.handle_write(port, width, val);
        }
        panic_device_not_found("port", port, false, width);
    }
}
