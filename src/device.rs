//! Device management for AxVM virtual machines.
//!
//! This module provides device abstraction and I/O handling for virtual machines.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use range_alloc::RangeAllocator;
use spin::Mutex;

use axaddrspace::{
    GuestPhysAddr, GuestPhysAddrRange,
    device::{AccessWidth, DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::{BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps};
use axerrno::{AxResult, AxError, ax_err};
use axvmconfig::{EmulatedDeviceConfig, EmulatedDeviceType};
use memory_addr::is_aligned_4k;

#[cfg(target_arch = "aarch64")]
use memory_addr::PhysAddr;

use crate::AxVmDeviceConfig;
use crate::registry::DeviceRegistry;
use crate::wrapper::DeviceId;
use crate::notify::{DeviceNotificationManager, DeviceNotifierImpl, PendingNotification};

// Re-export legacy types for backward compatibility
use crate::notify::DeviceInterruptManager;
use crate::notify::DeviceInterruptTrigger;
use crate::notify::PendingInterrupt;

#[cfg(target_arch = "aarch64")]
use arm_vgic::Vgic;

#[cfg(target_arch = "riscv64")]
use riscv_vplic::VPlicGlobal;

use crate::virtio::{new_dummy_virtio_device, new_dummy_uart_device, new_dummy_clint_device};

/// Error and informational messages used throughout the module.
mod msgs {
    /// Error message for invalid argument count.
    pub const ERR_INVALID_ARG_COUNT: &str = "Invalid argument count";

    /// GPPT Redistributor configuration errors.
    pub mod gppt_gicr {
        pub const ERR_ARG_COUNT: &str =
            "expect 3 args for gppt redistributor (cpu_num, stride, pcpu_id)";
    }

    /// GPPT ITS configuration errors.
    pub mod gppt_its {
        pub const ERR_ARG_COUNT: &str = "expect 1 arg for gppt its (host_gits_base)";
    }

    /// PPPT Global configuration errors.
    pub mod pppt_global {
        pub const ERR_ARG_COUNT: &str = "expect 1 arg for pppt global (context_num)";
    }

    /// IVC channel errors.
    pub mod ivc {
        pub const ERR_NOT_EXISTS: &str = "IVC channel not initialized";
        pub const ERR_INVALID_SIZE: &str = "Size must be greater than 0";
        pub const ERR_INVALID_ALIGN: &str = "Size must be aligned to 4K";
        pub const ERR_ALLOC_FAILED: &str = "IVC channel allocation failed";
    }

    /// Platform not supported errors.
    pub mod platform {
        pub const ERR_INTERRUPT_CONTROLLER: &str = "InterruptController not supported on this platform";
        pub const ERR_GPPT_REDISTRIBUTOR: &str = "GPPTRedistributor not supported on this platform";
        pub const ERR_GPPT_DISTRIBUTOR: &str = "GPPTDistributor not supported on this platform";
        pub const ERR_GPPT_ITS: &str = "GPPTITS not supported on this platform";
        pub const ERR_PPPT_GLOBAL: &str = "PPPTGlobal not supported on this platform";
    }
}

/// A collection of emulated devices accessible by a specific address range type.
///
/// This structure now uses a [`DeviceRegistry`] internally for:
/// - O(log n) device lookup using interval trees
/// - Per-device concurrency control
/// - Device lifecycle management (hot-plug/hot-unplug)
///
/// # Type Parameters
///
/// * `R` - The device address range type (e.g., `GuestPhysAddrRange`, `SysRegAddrRange`, `PortRange`)
///
/// # Examples
///
/// ```rust,ignore
/// use axdevice::AxEmuDevices;
/// use axaddrspace::GuestPhysAddrRange;
///
/// let mut devices = AxEmuDevices::<GuestPhysAddrRange>::new();
/// // Add devices...
/// let device_id = devices.add_dev(my_device);
/// // Later: remove device
/// devices.remove_dev(device_id);
/// ```
pub struct AxEmuDevices<R: DeviceAddrRange> {
    registry: DeviceRegistry<R>,
}

impl<R: DeviceAddrRange + Copy + PartialEq + 'static> AxEmuDevices<R> {
    /// Creates a new empty [`AxEmuDevices`] instance.
    pub fn new() -> Self {
        Self {
            registry: DeviceRegistry::new(),
        }
    }

    /// Adds a device to the collection.
    ///
    /// Returns the assigned device ID, which can be used later for removal.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The device has no address ranges
    /// - Any address range overlaps with an existing device
    pub fn add_dev(&mut self, dev: Arc<dyn BaseDeviceOps<R>>) -> AxResult<DeviceId> {
        self.registry.add_device(dev)
    }

    /// Removes a device from the collection.
    ///
    /// This performs a graceful removal:
    /// 1. Marks device as "Removing" (rejects new accesses)
    /// 2. Waits for active accesses to complete
    /// 3. Unregisters address ranges
    /// 4. Marks device as "Removed"
    ///
    /// # Errors
    ///
    /// Returns an error if the device ID is not found or already removed.
    pub fn remove_dev(&mut self, id: DeviceId) -> AxResult {
        self.registry.remove_device(id)
    }

    /// Handles a read operation at the specified address.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No device is registered for this address
    /// - The device is not active (being removed or removed)
    /// - The device's read handler returns an error
    fn handle_read(&self, addr: R::Addr, width: AccessWidth) -> AxResult<usize> {
        self.registry.handle_read(addr, width)
    }

    /// Handles a write operation at the specified address.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No device is registered for this address
    /// - The device is not active (being removed or removed)
    /// - The device's write handler returns an error
    fn handle_write(&self, addr: R::Addr, width: AccessWidth, val: usize) -> AxResult {
        self.registry.handle_write(addr, width, val)
    }

    /// Lists all registered device IDs.
    pub fn list_devices(&self) -> Vec<DeviceId> {
        self.registry.list_devices()
    }

    /// Gets the number of registered devices.
    pub fn device_count(&self) -> usize {
        self.registry.device_count()
    }
}

type AxEmuMmioDevices = AxEmuDevices<GuestPhysAddrRange>;
type AxEmuSysRegDevices = AxEmuDevices<SysRegAddrRange>;
type AxEmuPortDevices = AxEmuDevices<PortRange>;

/// Device manager for a single virtual machine.
///
/// Manages all emulated devices for a VM, including:
/// - MMIO devices (memory-mapped I/O)
/// - System register devices (ARM system registers)
/// - Port devices (x86 I/O ports)
/// - IVC channels (Inter-VM Communication)
/// - Interrupt management
///
/// # Examples
///
/// ```rust,ignore
/// use axdevice::{AxVmDevices, AxVmDeviceConfig};
///
/// // Create device manager from configuration
/// let config = AxVmDeviceConfig::new(vec![...]);
/// let mut devices = AxVmDevices::new(config);
///
/// // Initialize interrupt manager for 4 vCPUs
/// devices.init_interrupt_manager(4);
///
/// // Handle MMIO read
/// let value = devices.handle_mmio_read(gpa, AccessWidth::Bits32)?;
///
/// // Allocate IVC channel
/// let channel_addr = devices.alloc_ivc_channel(0x1000)?;
///
/// // Pop pending interrupt before VM entry
/// if let Some(irq) = devices.pop_pending_interrupt(cpu_id) {
///     vcpu.inject_interrupt(irq.irq);
/// }
/// ```
pub struct AxVmDevices {
    /// MMIO devices (memory-mapped I/O)
    emu_mmio_devices: AxEmuMmioDevices,
    /// System register devices
    emu_sys_reg_devices: AxEmuSysRegDevices,
    /// Port devices (x86 I/O ports)
    emu_port_devices: AxEmuPortDevices,
    /// IVC channel range allocator for inter-VM communication
    ivc_channel: Option<Mutex<RangeAllocator<usize>>>,
    /// Interrupt manager for device interrupts
    interrupt_manager: Option<Arc<DeviceInterruptManager>>,
}

#[inline]
fn log_device_io(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    addr_ranges: &[impl core::fmt::LowerHex],
    read: bool,
    width: AccessWidth,
) {
    let rw = if read { "read" } else { "write" };
    if addr_ranges.len() == 1 {
        trace!(
            "emu_device {}: {} {:#x} in range {:#x} with width {:?}",
            rw, addr_type, addr, addr_ranges[0], width
        )
    } else {
        // For multi-range devices, log all ranges
        let ranges_str: alloc::string::String = addr_ranges
            .iter()
            .map(|r| alloc::format!("{:#x}", r))
            .collect::<Vec<_>>()
            .join(", ");
        trace!(
            "emu_device {}: {} {:#x} in ranges [{}] with width {:?}",
            rw, addr_type, addr, ranges_str, width
        )
    }
}

/// Implementation for AxVmDevices
impl AxVmDevices {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> Self {
        let mut this = Self {
            emu_mmio_devices: AxEmuMmioDevices::new(),
            emu_sys_reg_devices: AxEmuSysRegDevices::new(),
            emu_port_devices: AxEmuPortDevices::new(),
            ivc_channel: None,
            interrupt_manager: None,
        };

        Self::init(&mut this, &config.emu_configs);
        this
    }

    /// Initialize specific devices according to the emu_configs
    fn init(this: &mut Self, emu_configs: &[EmulatedDeviceConfig]) {
        for config in emu_configs {
            if let Err(e) = this.init_device(config) {
                warn!(
                    "Failed to initialize device '{}': {:?}",
                    config.name, e
                );
            }
        }
    }

    /// Initialize a single device based on its configuration.
    fn init_device(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        match config.emu_type {
            #[cfg(target_arch = "aarch64")]
            EmulatedDeviceType::InterruptController => self.init_interrupt_controller(),
            #[cfg(target_arch = "aarch64")]
            EmulatedDeviceType::GPPTRedistributor => self.init_gppt_redistributor(config),
            #[cfg(target_arch = "aarch64")]
            EmulatedDeviceType::GPPTDistributor => self.init_gppt_distributor(config),
            #[cfg(target_arch = "aarch64")]
            EmulatedDeviceType::GPPTITS => self.init_gppt_its(config),
            #[cfg(target_arch = "riscv64")]
            EmulatedDeviceType::PPPTGlobal => self.init_pppt_global(config),
            EmulatedDeviceType::IVCChannel => self.init_ivc_channel(config),
            EmulatedDeviceType::Console => self.init_console_device(config),
            EmulatedDeviceType::Dummy => self.init_dummy_device(config),
            EmulatedDeviceType::VirtioBlk | EmulatedDeviceType::VirtioNet | EmulatedDeviceType::VirtioConsole => {
                // Skip dummy device for VirtioBlk when virtio-blk feature is enabled
                // (real device will be created later in init_virtio_blk)
                #[cfg(feature = "virtio-blk")]
                if config.emu_type == EmulatedDeviceType::VirtioBlk {
                    debug!("Skipping dummy device for VirtioBlk (will be initialized with real device later)");
                    return Ok(());
                }
                // Skip dummy device for VirtioConsole when virtio-console feature is enabled
                // (real device will be created later in init_virtio_console)
                #[cfg(feature = "virtio-console")]
                if config.emu_type == EmulatedDeviceType::VirtioConsole {
                    debug!("Skipping dummy device for VirtioConsole (will be initialized with real device later)");
                    return Ok(());
                }
                self.init_virtio_device(config)
            }
            _ => {
                warn!(
                    "Emulated device '{}' type {:?} is not supported yet",
                    config.name, config.emu_type
                );
                Ok(())
            }
        }
    }

    /// Initialize the interrupt controller (ARM64 only).
    #[cfg(target_arch = "aarch64")]
    fn init_interrupt_controller(&mut self) -> AxResult {
        self.add_mmio_dev(Arc::new(Vgic::new()));
        Ok(())
    }

    /// Initialize GPPT redistributors (ARM64 only).
    #[cfg(target_arch = "aarch64")]
    fn init_gppt_redistributor(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        let cpu_num = config
            .cfg_list
            .first()
            .copied()
            .ok_or_else(|| ax_err!(InvalidInput, msgs::gppt_gicr::ERR_ARG_COUNT))?;
        let stride = config
            .cfg_list
            .get(1)
            .copied()
            .ok_or_else(|| ax_err!(InvalidInput, msgs::gppt_gicr::ERR_ARG_COUNT))?;
        let pcpu_id = config
            .cfg_list
            .get(2)
            .copied()
            .ok_or_else(|| ax_err!(InvalidInput, msgs::gppt_gicr::ERR_ARG_COUNT))?;

        for i in 0..cpu_num {
            let addr = config.base_gpa + i * stride;
            let size = config.length;
            self.add_mmio_dev(Arc::new(arm_vgic::v3::vgicr::VGicR::new(
                addr.into(),
                Some(size),
                pcpu_id + i,
            )));

            info!(
                "GPPT Redistributor initialized for vCPU {} with base GPA {:#x} and length {:#x}",
                i, addr, size
            );
        }
        Ok(())
    }

    /// Initialize GPPT distributor (ARM64 only).
    #[cfg(target_arch = "aarch64")]
    fn init_gppt_distributor(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        self.add_mmio_dev(Arc::new(arm_vgic::v3::vgicd::VGicD::new(
            config.base_gpa.into(),
            Some(config.length),
        )));

        info!(
            "GPPT Distributor initialized with base GPA {:#x} and length {:#x}",
            config.base_gpa, config.length
        );
        Ok(())
    }

    /// Initialize GPPT ITS (ARM64 only).
    #[cfg(target_arch = "aarch64")]
    fn init_gppt_its(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        let host_gits_base = config
            .cfg_list
            .first()
            .copied()
            .map(PhysAddr::from_usize)
            .ok_or_else(|| ax_err!(InvalidInput, msgs::gppt_its::ERR_ARG_COUNT))?;

        self.add_mmio_dev(Arc::new(arm_vgic::v3::gits::Gits::new(
            config.base_gpa.into(),
            Some(config.length),
            host_gits_base,
            false,
        )));

        info!(
            "GPPT ITS initialized with base GPA {:#x} and length {:#x}, host GITS base {:#x}",
            config.base_gpa, config.length, host_gits_base
        );
        Ok(())
    }

    /// Initialize PPPT global PLIC (RISC-V only).
    #[cfg(target_arch = "riscv64")]
    fn init_pppt_global(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        let context_num = config
            .cfg_list
            .first()
            .copied()
            .ok_or_else(|| axerrno::ax_err_type!(InvalidInput, msgs::pppt_global::ERR_ARG_COUNT))?;

        self.add_mmio_dev(Arc::new(VPlicGlobal::new(
            config.base_gpa.into(),
            Some(config.length),
            context_num,
        )));

        info!(
            "Partial PLIC Passthrough Global initialized with base GPA {:#x} and length {:#x}",
            config.base_gpa, config.length
        );
        Ok(())
    }

    /// Initialize IVC channel allocator.
    fn init_ivc_channel(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        if self.ivc_channel.is_some() {
            warn!("IVCChannel already initialized, ignoring additional config");
            return Ok(());
        }

        self.ivc_channel = Some(Mutex::new(RangeAllocator::new(Range {
            start: config.base_gpa,
            end: config.base_gpa + config.length,
        })));

        info!(
            "IVCChannel initialized with base GPA {:#x} and length {:#x}",
            config.base_gpa, config.length
        );
        Ok(())
    }

    /// Initialize a dummy virtio device.
    ///
    /// This creates a minimal virtio device that responds to MMIO accesses
    /// without providing actual device functionality. It's useful for preventing
    /// VM crashes when the guest OS tries to access unimplemented virtio devices.
    fn init_virtio_device(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        let device = new_dummy_virtio_device(config.base_gpa.into(), Some(config.length));
        self.add_mmio_dev(device);

        info!(
            "Dummy virtio device '{}' initialized with base GPA {:#x} and length {:#x}",
            config.name, config.base_gpa, config.length
        );
        Ok(())
    }

    /// Initialize a dummy console (UART) device.
    ///
    /// This creates a minimal UART device that responds to MMIO accesses
    /// without providing actual device functionality. It's useful for preventing
    /// VM crashes when the guest OS tries to access unimplemented UART devices.
    fn init_console_device(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        let device = new_dummy_uart_device(config.base_gpa.into(), Some(config.length));
        self.add_mmio_dev(device);

        info!(
            "Dummy console device '{}' initialized with base GPA {:#x} and length {:#x}",
            config.name, config.base_gpa, config.length
        );
        Ok(())
    }

    /// Initialize a dummy device (e.g., CLINT).
    ///
    /// This creates a minimal dummy device that responds to MMIO accesses
    /// without providing actual device functionality. It's useful for preventing
    /// VM crashes when the guest OS tries to access unimplemented devices.
    fn init_dummy_device(&mut self, config: &EmulatedDeviceConfig) -> AxResult {
        let device = new_dummy_clint_device(config.base_gpa.into(), Some(config.length));
        self.add_mmio_dev(device);

        info!(
            "Dummy device '{}' initialized with base GPA {:#x} and length {:#x}",
            config.name, config.base_gpa, config.length
        );
        Ok(())
    }

    /// Platform-specific device initialization stubs.
    #[cfg(not(target_arch = "aarch64"))]
    fn init_interrupt_controller(&mut self) -> AxResult {
        ax_err!(Unsupported, msgs::platform::ERR_INTERRUPT_CONTROLLER)
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn init_gppt_redistributor(&mut self, _config: &EmulatedDeviceConfig) -> AxResult {
        ax_err!(Unsupported, msgs::platform::ERR_GPPT_REDISTRIBUTOR)
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn init_gppt_distributor(&mut self, _config: &EmulatedDeviceConfig) -> AxResult {
        ax_err!(Unsupported, msgs::platform::ERR_GPPT_DISTRIBUTOR)
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn init_gppt_its(&mut self, _config: &EmulatedDeviceConfig) -> AxResult {
        ax_err!(Unsupported, msgs::platform::ERR_GPPT_ITS)
    }

    #[cfg(not(target_arch = "riscv64"))]
    fn init_pppt_global(&mut self, _config: &EmulatedDeviceConfig) -> AxResult {
        ax_err!(Unsupported, msgs::platform::ERR_PPPT_GLOBAL)
    }

    /// Allocates an IVC (Inter-VM Communication) channel of the specified size.
    pub fn alloc_ivc_channel(&self, size: usize) -> AxResult<GuestPhysAddr> {
        if size == 0 {
            return ax_err!(InvalidInput, msgs::ivc::ERR_INVALID_SIZE);
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, msgs::ivc::ERR_INVALID_ALIGN);
        }

        if let Some(allocator) = &self.ivc_channel {
            allocator
                .lock()
                .allocate_range(size)
                .map_err(|e| {
                    warn!("Failed to allocate IVC channel range: {:x?}", e);
                    axerrno::ax_err_type!(NoMemory, msgs::ivc::ERR_ALLOC_FAILED)
                })
                .map(|range| {
                    debug!("Allocated IVC channel range: {:x?}", range);
                    GuestPhysAddr::from_usize(range.start)
                })
        } else {
            ax_err!(InvalidInput, msgs::ivc::ERR_NOT_EXISTS)
        }
    }

    /// Releases an IVC channel at the specified address and size.
    pub fn release_ivc_channel(&self, addr: GuestPhysAddr, size: usize) -> AxResult {
        if size == 0 {
            return ax_err!(InvalidInput, msgs::ivc::ERR_INVALID_SIZE);
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, msgs::ivc::ERR_INVALID_ALIGN);
        }

        if let Some(allocator) = &self.ivc_channel {
            allocator
                .lock()
                .free_range(addr.as_usize()..addr.as_usize() + size);
            Ok(())
        } else {
            ax_err!(InvalidInput, msgs::ivc::ERR_NOT_EXISTS)
        }
    }

    /// Add a MMIO device to the device list
    ///
    /// # Panics
    ///
    /// Panics if the device cannot be added (e.g., address range overlap).
    /// For error handling, use `try_add_mmio_dev`.
    pub fn add_mmio_dev(&mut self, dev: Arc<dyn BaseMmioDeviceOps>) {
        self.emu_mmio_devices.add_dev(dev).expect("Failed to add MMIO device");
    }

    /// Try to add a MMIO device to the device list
    ///
    /// Returns the assigned device ID on success.
    ///
    /// If the device supports interrupts (via `interrupt_config()`), this method will:
    /// 1. Register the interrupt configuration with the interrupt manager
    /// 2. Create an interrupt trigger
    /// 3. Inject the trigger into the device via `set_interrupt_trigger()`
    pub fn try_add_mmio_dev(&mut self, dev: Arc<dyn BaseMmioDeviceOps>) -> AxResult<DeviceId> {
        let device_id = self.emu_mmio_devices.add_dev(dev.clone())?;

        // If device has interrupt support and interrupt manager is initialized
        if let Some(config) = dev.interrupt_config() {
            if let Some(manager) = &self.interrupt_manager {
                // Register interrupt configuration
                manager.register(device_id, config.clone())?;

                // Create trigger and inject into device
                let trigger = Arc::new(DeviceInterruptTrigger::new(
                    device_id,
                    Arc::clone(manager),
                ));
                dev.set_interrupt_trigger(trigger);
            }
        }

        Ok(device_id)
    }

    /// Add a system register device to the device list
    ///
    /// # Panics
    ///
    /// Panics if the device cannot be added (e.g., address range overlap).
    /// For error handling, use `try_add_sys_reg_dev`.
    pub fn add_sys_reg_dev(&mut self, dev: Arc<dyn BaseSysRegDeviceOps>) {
        self.emu_sys_reg_devices.add_dev(dev).expect("Failed to add sysreg device");
    }

    /// Try to add a system register device to the device list
    ///
    /// Returns the assigned device ID on success.
    ///
    /// If the device supports interrupts, registers them with the interrupt manager.
    pub fn try_add_sys_reg_dev(&mut self, dev: Arc<dyn BaseSysRegDeviceOps>) -> AxResult<DeviceId> {
        let device_id = self.emu_sys_reg_devices.add_dev(dev.clone())?;

        // Register interrupt if supported
        if let Some(config) = dev.interrupt_config() {
            if let Some(manager) = &self.interrupt_manager {
                manager.register(device_id, config)?;
                let trigger = Arc::new(DeviceInterruptTrigger::new(
                    device_id,
                    Arc::clone(manager),
                ));
                dev.set_interrupt_trigger(trigger);
            }
        }

        Ok(device_id)
    }

    /// Add a port device to the device list
    ///
    /// # Panics
    ///
    /// Panics if the device cannot be added (e.g., address range overlap).
    /// For error handling, use `try_add_port_dev`.
    pub fn add_port_dev(&mut self, dev: Arc<dyn BasePortDeviceOps>) {
        self.emu_port_devices.add_dev(dev).expect("Failed to add port device");
    }

    /// Try to add a port device to the device list
    ///
    /// Returns the assigned device ID on success.
    ///
    /// If the device supports interrupts, registers them with the interrupt manager.
    pub fn try_add_port_dev(&mut self, dev: Arc<dyn BasePortDeviceOps>) -> AxResult<DeviceId> {
        let device_id = self.emu_port_devices.add_dev(dev.clone())?;

        // Register interrupt if supported
        if let Some(config) = dev.interrupt_config() {
            if let Some(manager) = &self.interrupt_manager {
                manager.register(device_id, config)?;
                let trigger = Arc::new(DeviceInterruptTrigger::new(
                    device_id,
                    Arc::clone(manager),
                ));
                dev.set_interrupt_trigger(trigger);
            }
        }

        Ok(device_id)
    }

    /// Remove a MMIO device from the device list
    ///
    /// Also unregisters the device's interrupt if it was registered.
    pub fn remove_mmio_dev(&mut self, id: DeviceId) -> AxResult {
        // Unregister interrupt if manager exists
        if let Some(manager) = &self.interrupt_manager {
            let _ = manager.unregister(id); // Ignore error if not registered
        }

        self.emu_mmio_devices.remove_dev(id)
    }

    /// Remove a system register device from the device list
    ///
    /// Also unregisters the device's interrupt if it was registered.
    pub fn remove_sys_reg_dev(&mut self, id: DeviceId) -> AxResult {
        if let Some(manager) = &self.interrupt_manager {
            let _ = manager.unregister(id);
        }

        self.emu_sys_reg_devices.remove_dev(id)
    }

    /// Remove a port device from the device list
    ///
    /// Also unregisters the device's interrupt if it was registered.
    pub fn remove_port_dev(&mut self, id: DeviceId) -> AxResult {
        if let Some(manager) = &self.interrupt_manager {
            let _ = manager.unregister(id);
        }

        self.emu_port_devices.remove_dev(id)
    }

    /// Lists all MMIO device IDs.
    pub fn list_mmio_devices(&self) -> Vec<DeviceId> {
        self.emu_mmio_devices.list_devices()
    }

    /// Lists all system register device IDs.
    pub fn list_sys_reg_devices(&self) -> Vec<DeviceId> {
        self.emu_sys_reg_devices.list_devices()
    }

    /// Lists all port device IDs.
    pub fn list_port_devices(&self) -> Vec<DeviceId> {
        self.emu_port_devices.list_devices()
    }

    /// Handle the MMIO read by GuestPhysAddr and data width.
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        self.emu_mmio_devices.handle_read(addr, width).map_err(|e| {
            if e == AxError::NotFound {
                error!("mmio device not found for address {:#x}", addr);
            }
            e
        })
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value.
    pub fn handle_mmio_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        self.emu_mmio_devices.handle_write(addr, width, val).map_err(|e| {
            if e == AxError::NotFound {
                error!("mmio device not found for address {:#x}", addr);
            }
            e
        })
    }

    /// Handle the system register read by SysRegAddr and data width.
    pub fn handle_sys_reg_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        self.emu_sys_reg_devices.handle_read(addr, width).map_err(|e| {
            if e == AxError::NotFound {
                error!("sys_reg device not found for address {:#x}", addr.0);
            }
            e
        })
    }

    /// Handle the system register write by SysRegAddr, data width and the value.
    pub fn handle_sys_reg_write(&self, addr: SysRegAddr, width: AccessWidth, val: usize) -> AxResult {
        self.emu_sys_reg_devices.handle_write(addr, width, val).map_err(|e| {
            if e == AxError::NotFound {
                error!("sys_reg device not found for address {:#x}", addr.0);
            }
            e
        })
    }

    /// Handle the port read by port number and data width.
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        self.emu_port_devices.handle_read(port, width).map_err(|e| {
            if e == AxError::NotFound {
                error!("port device not found for port {:#x}", port.0);
            }
            e
        })
    }

    /// Handle the port write by port number, data width and the value.
    pub fn handle_port_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        self.emu_port_devices.handle_write(port, width, val).map_err(|e| {
            if e == AxError::NotFound {
                error!("port device not found for port {:#x}", port.0);
            }
            e
        })
    }

    // ========================================================================
    // Interrupt Management Methods
    // ========================================================================

    /// Initializes the interrupt manager for this VM.
    ///
    /// Must be called before adding devices that require interrupt support.
    ///
    /// # Arguments
    ///
    /// * `cpu_count` - Number of vCPUs in the VM.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut devices = AxVmDevices::new(config);
    /// devices.init_interrupt_manager(4); // 4 vCPUs
    /// ```
    pub fn init_interrupt_manager(&mut self, cpu_count: usize) {
        info!("Initializing interrupt manager for {} vCPUs", cpu_count);
        self.interrupt_manager = Some(Arc::new(DeviceInterruptManager::new(cpu_count)));
    }

    /// Pops the highest-priority pending interrupt for a vCPU.
    ///
    /// This should be called before VM entry to inject pending interrupts.
    ///
    /// # Arguments
    ///
    /// * `cpu_id` - The vCPU ID.
    ///
    /// # Returns
    ///
    /// The highest-priority pending interrupt, or `None` if no interrupts are pending.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Before VM entry
    /// if let Some(irq) = devices.pop_pending_interrupt(cpu_id) {
    ///     vcpu.inject_interrupt(irq.irq);
    /// }
    /// ```
    pub fn pop_pending_interrupt(&self, cpu_id: usize) -> Option<PendingInterrupt> {
        self.interrupt_manager.as_ref()?.pop_pending(cpu_id)
    }

    /// Gets the number of pending interrupts for a vCPU.
    pub fn pending_interrupt_count(&self, cpu_id: usize) -> usize {
        self.interrupt_manager
            .as_ref()
            .map(|m| m.pending_count(cpu_id))
            .unwrap_or(0)
    }

    /// Injects a passthrough device interrupt.
    ///
    /// This method is used to inject interrupts from passthrough devices
    /// (e.g., physical UART, network cards) into the interrupt queue.
    /// The interrupt will be delivered to the guest when the vCPU runs next.
    ///
    /// # Arguments
    ///
    /// * `irq` - The IRQ number from the physical device.
    /// * `cpu_id` - The target vCPU ID.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // In external interrupt handler
    /// let irq = plic.claim();
    /// devices.inject_passthrough_interrupt(irq, current_cpu_id);
    /// ```
    pub fn inject_passthrough_interrupt(&self, irq: u32, cpu_id: usize) -> AxResult {
        const PASSTHROUGH_PRIORITY: u8 = 50; // Default priority for passthrough devices

        if let Some(manager) = &self.interrupt_manager {
            manager.inject_raw(irq, cpu_id, PASSTHROUGH_PRIORITY)
        } else {
            log::warn!("inject_passthrough_interrupt: interrupt manager not initialized");
            Ok(())
        }
    }

    /// Gets the interrupt manager reference.
    ///
    /// Returns `None` if the interrupt manager has not been initialized.
    pub fn interrupt_manager(&self) -> Option<&Arc<DeviceInterruptManager>> {
        self.interrupt_manager.as_ref()
    }

    /// Clears all pending interrupts for all vCPUs.
    ///
    /// Useful for VM reset operations.
    pub fn clear_all_pending_interrupts(&self) {
        if let Some(manager) = &self.interrupt_manager {
            manager.clear_all_pending();
        }
    }
}

