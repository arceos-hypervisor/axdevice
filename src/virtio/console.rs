//! VirtIO Console device adapter for axdevice framework.
//!
//! This module provides an adapter that wraps the `VirtioMmioConsoleDevice` from
//! axvirtio-console and implements the `BaseDeviceOps` trait from axdevice_base,
//! enabling seamless integration with the AxVisor device management system.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │              AxVmDevices                    │
//! │         (Device Registry)                   │
//! └─────────────────┬───────────────────────────┘
//!                   │ BaseDeviceOps trait
//!                   ▼
//! ┌─────────────────────────────────────────────┐
//! │       VirtioConsoleDevice<B, T>             │
//! │            (This Adapter)                   │
//! │   - Implements BaseDeviceOps                │
//! │   - Wraps VirtioMmioConsoleDevice           │
//! │   - Handles interrupt triggering            │
//! └─────────────────┬───────────────────────────┘
//!                   │
//!                   ▼
//! ┌─────────────────────────────────────────────┐
//! │     VirtioMmioConsoleDevice<B, T>           │
//! │         (from axvirtio-console)             │
//! │   - VirtIO MMIO transport                   │
//! │   - Console I/O processing                  │
//! └─────────────────┬───────────────────────────┘
//!                   │
//!                   ▼
//! ┌─────────────────────────────────────────────┐
//! │           ConsoleBackend                    │
//! │   (User-provided console backend)           │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use axdevice::virtio::VirtioConsoleDevice;
//! use axvirtio_console::ConsoleBackend;
//!
//! // Create a console backend (e.g., UART-backed or memory-backed)
//! let backend = MyConsoleBackend::new();
//!
//! // Create the VirtIO console device
//! let device = VirtioConsoleDevice::new(
//!     base_addr,
//!     size,
//!     backend,
//!     console_config,
//!     memory_accessor,
//!     irq_id,
//! )?;
//!
//! // Add to the VM's device list
//! vm_devices.try_add_mmio_dev(Arc::new(device))?;
//! ```

use alloc::sync::Arc;
use spin::RwLock;

use axaddrspace::{device::AccessWidth, GuestMemoryAccessor, GuestPhysAddr, GuestPhysAddrRange};
use axdevice_base::{
    BaseDeviceOps, CpuAffinity, EmuDeviceType, InterruptConfig, InterruptTrigger, IrqType,
    TriggerMode,
};
use axerrno::AxResult;

use axvirtio_console::{ConsoleBackend, VirtioConsoleConfig, VirtioMmioConsoleDevice};

use crate::ConsoleInputHandler;

/// VirtIO Console device adapter for axdevice framework.
///
/// This struct wraps the `VirtioMmioConsoleDevice` from axvirtio-console and implements
/// the `BaseDeviceOps` trait, enabling integration with the AxVisor device
/// management system.
///
/// # Type Parameters
///
/// * `B` - Console backend implementation that handles actual I/O operations
/// * `T` - Guest memory accessor with address translation capabilities
///
/// # Interrupt Handling
///
/// The device supports interrupt triggering through the `InterruptTrigger` trait.
/// When the VirtIO device needs to signal the guest (e.g., after receiving input),
/// it calls `trigger_interrupt()` which uses the injected trigger.
pub struct VirtioConsoleDevice<B: ConsoleBackend, T: GuestMemoryAccessor + Clone + Send + Sync> {
    /// The underlying VirtIO MMIO console device.
    inner: VirtioMmioConsoleDevice<B, T>,
    /// The address range of this device.
    range: GuestPhysAddrRange,
    /// Interrupt trigger injected by the device framework.
    /// Uses RwLock for thread-safe interior mutability.
    interrupt_trigger: RwLock<Option<Arc<dyn InterruptTrigger>>>,
    /// Interrupt configuration.
    irq_id: u32,
}

impl<B: ConsoleBackend + 'static, T: GuestMemoryAccessor + Clone + Send + Sync + 'static>
    VirtioConsoleDevice<B, T>
{
    /// Creates a new VirtIO Console device.
    ///
    /// # Arguments
    ///
    /// * `base_ipa` - Base guest physical address of the device's MMIO region
    /// * `length` - Size of the MMIO region in bytes
    /// * `backend` - Console backend for I/O operations
    /// * `console_config` - VirtIO console device configuration
    /// * `accessor` - Guest memory accessor for address translation
    /// * `irq_id` - Interrupt request ID for this device
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying VirtIO device creation fails.
    pub fn new(
        base_ipa: GuestPhysAddr,
        length: usize,
        backend: B,
        console_config: VirtioConsoleConfig,
        accessor: T,
        irq_id: u32,
    ) -> AxResult<Self> {
        let inner = VirtioMmioConsoleDevice::new(base_ipa, length, backend, console_config, accessor)
            .map_err(|_| axerrno::ax_err_type!(BadState, "Failed to create VirtIO console device"))?;

        let end = base_ipa + length;
        let range = (base_ipa.as_usize()..end.as_usize())
            .try_into()
            .map_err(|_| axerrno::ax_err_type!(InvalidInput, "Invalid address range"))?;

        Ok(Self {
            inner,
            range,
            interrupt_trigger: RwLock::new(None),
            irq_id,
        })
    }

    /// Checks if the device is ready.
    pub fn is_device_ready(&self) -> bool {
        self.inner.is_device_ready()
    }

    /// Gets the interrupt status.
    pub fn get_interrupt_status(&self) -> u32 {
        self.inner.get_interrupt_status()
    }

    /// Triggers an interrupt to the guest.
    ///
    /// This is called by the VirtIO device when it needs to notify the guest
    /// (e.g., after receiving input from the host).
    pub fn trigger_interrupt(&self) -> AxResult {
        if let Some(trigger) = self.interrupt_trigger.read().as_ref() {
            trigger.trigger(IrqType::Primary)
        } else {
            Ok(())
        }
    }

    /// Push input data to the console (host to guest).
    ///
    /// This method is called when there is input from the host terminal
    /// that should be forwarded to the guest.
    ///
    /// # Arguments
    ///
    /// * `data` - The input data to push to the guest
    ///
    /// # Returns
    ///
    /// The number of bytes successfully pushed to the guest.
    pub fn push_input(&self, data: &[u8]) -> AxResult<usize> {
        let written = self
            .inner
            .push_input(data)
            .map_err(|_| axerrno::ax_err_type!(BadState, "Failed to push input to console"))?;

        // If we wrote data and device is ready, trigger interrupt
        if written > 0 && self.is_device_ready() {
            let int_status = self.get_interrupt_status();
            if int_status != 0 {
                self.trigger_interrupt()?;
            }
        }

        Ok(written)
    }
}

impl<B: ConsoleBackend + 'static, T: GuestMemoryAccessor + Clone + Send + Sync + 'static>
    BaseDeviceOps<GuestPhysAddrRange> for VirtioConsoleDevice<B, T>
{
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::VirtioConsole
    }

    fn address_ranges(&self) -> &[GuestPhysAddrRange] {
        core::slice::from_ref(&self.range)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        self.inner
            .mmio_read(addr, width)
            .map_err(|_| axerrno::ax_err_type!(BadState, "VirtIO read failed"))
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        self.inner
            .mmio_write(addr, width, val)
            .map_err(|_| axerrno::ax_err_type!(BadState, "VirtIO write failed"))?;

        // After MMIO write, check if the device triggered an interrupt
        let int_status = self.get_interrupt_status();

        // The underlying device (axvirtio-console) determines whether to trigger
        // an interrupt based on VirtIO spec (checking VIRTQ_AVAIL_F_NO_INTERRUPT flag).
        // We simply forward the interrupt request to the interrupt manager.
        if int_status != 0 {
            self.trigger_interrupt()?;
        }

        Ok(())
    }

    fn interrupt_config(&self) -> Option<InterruptConfig> {
        Some(InterruptConfig {
            primary_irq: self.irq_id,
            additional_irqs: alloc::vec![],
            trigger_mode: TriggerMode::Level,
            cpu_affinity: CpuAffinity::Fixed(0),
            priority: 100,
        })
    }

    fn set_interrupt_trigger(&self, trigger: Arc<dyn InterruptTrigger>) {
        *self.interrupt_trigger.write() = Some(trigger);
    }
}

/// Implementation of ConsoleInputHandler for VirtioConsoleDevice.
///
/// This allows the hypervisor to forward input from the backend to the
/// guest without going through the normal MMIO path.
impl<B: ConsoleBackend + 'static, T: GuestMemoryAccessor + Clone + Send + Sync + 'static>
    ConsoleInputHandler for VirtioConsoleDevice<B, T>
{
    fn forward_input(&self) -> usize {
        // Read from backend and push to guest's VirtIO receiveq
        let forwarded = self.inner.forward_backend_input();

        // Trigger interrupt to notify guest if we forwarded data
        if forwarded > 0 {
            let _ = self.trigger_interrupt();
        }

        forwarded
    }
}

/// Configuration builder for VirtIO Console device.
///
/// Provides a fluent interface for configuring and creating a VirtIO Console device.
///
/// # Example
///
/// ```rust,ignore
/// let device = VirtioConsoleDeviceBuilder::new()
///     .base_address(0x0a001000.into())
///     .size(0x200)
///     .irq(33)
///     .terminal_size(80, 25)
///     .build(backend, accessor)?;
/// ```
pub struct VirtioConsoleDeviceBuilder {
    base_ipa: Option<GuestPhysAddr>,
    length: usize,
    irq_id: u32,
    console_config: VirtioConsoleConfig,
}

impl Default for VirtioConsoleDeviceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioConsoleDeviceBuilder {
    /// Creates a new builder with default values.
    pub fn new() -> Self {
        Self {
            base_ipa: None,
            length: 0x200, // 512 bytes default
            irq_id: 1,
            console_config: VirtioConsoleConfig::default(),
        }
    }

    /// Sets the base MMIO address.
    pub fn base_address(mut self, addr: GuestPhysAddr) -> Self {
        self.base_ipa = Some(addr);
        self
    }

    /// Sets the MMIO region size.
    pub fn size(mut self, size: usize) -> Self {
        self.length = size;
        self
    }

    /// Sets the IRQ ID.
    pub fn irq(mut self, irq_id: u32) -> Self {
        self.irq_id = irq_id;
        self
    }

    /// Sets the terminal size (columns and rows).
    pub fn terminal_size(mut self, cols: u16, rows: u16) -> Self {
        self.console_config = VirtioConsoleConfig::with_size(cols, rows);
        self
    }

    /// Builds the VirtIO Console device.
    ///
    /// # Arguments
    ///
    /// * `backend` - Console backend for I/O operations
    /// * `accessor` - Guest memory accessor
    ///
    /// # Errors
    ///
    /// Returns an error if the base address is not set or device creation fails.
    pub fn build<B, T>(self, backend: B, accessor: T) -> AxResult<VirtioConsoleDevice<B, T>>
    where
        B: ConsoleBackend + 'static,
        T: GuestMemoryAccessor + Clone + Send + Sync + 'static,
    {
        let base_ipa = self
            .base_ipa
            .ok_or_else(|| axerrno::ax_err_type!(InvalidInput, "Base address not set"))?;

        VirtioConsoleDevice::new(
            base_ipa,
            self.length,
            backend,
            self.console_config,
            accessor,
            self.irq_id,
        )
    }
}
