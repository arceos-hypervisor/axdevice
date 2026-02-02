#![no_std]

//! # AxVM Device Management
//!
//! This crate provides device abstraction and I/O handling for ArceOS virtual machines.
//! It is designed for `no_std` environments and uses the `alloc` crate for dynamic memory allocation.
//!
//! ## Architecture
//!
//! The module is organized into multiple layers:
//!
//! ### Core Infrastructure
//! - [`DeviceCell`]: Zero-cost interior mutability for device state
//! - [`DeviceLifecycle`]: State machine for device lifecycle management (Active/Removing/Removed)
//! - [`DeviceWrapper`]: Wrapper providing lifecycle tracking, locking, and statistics
//! - [`DeviceRegistry`]: O(log n) device lookup using interval trees
//!
//! ### Interrupt Management
//! - [`DeviceInterruptManager`]: Unified interrupt routing and queueing
//! - [`DeviceInterruptTrigger`]: Interrupt trigger implementation for devices
//! - [`PendingInterrupt`]: Priority-ordered pending interrupt queue
//!
//! ### High-Level API
//! - [`AxVmDeviceConfig`]: Device configuration management
//! - [`AxVmDevices`]: Device management and I/O handling
//! - [`DeviceId`]: Unique identifier for devices
//!
//! ## Features
//!
//! - **Thread Safety**: Per-device locks enable safe concurrent access
//! - **Performance**: O(log n) device lookup (vs O(n) linear search)
//! - **Hot-plug Support**: Dynamic device addition and removal
//! - **Statistics**: Per-device operation counters
//! - **Interrupt Management**: Unified interrupt system with priority queuing
//!
//! ## Supported Device Types
//!
//! | Platform | Device Types |
//! |----------|--------------|
//! | aarch64  | InterruptController (Vgic), GPPTRedistributor, GPPTDistributor, GPPTITS |
//! | riscv64  | PPPTGlobal (PLIC Partial Passthrough) |
//! | All      | IVCChannel (Inter-VM Communication) |
//!
//! ## Examples
//!
//! ### Basic Device Management
//!
//! ```rust,ignore
//! use axdevice::{AxVmDevices, AxVmDeviceConfig, DeviceId};
//! use axaddrspace::{GuestPhysAddr, device::AccessWidth};
//!
//! // Create device configuration
//! let config = AxVmDeviceConfig::new(vec![...]);
//!
//! // Initialize device manager
//! let mut devices = AxVmDevices::new(config);
//!
//! // Initialize interrupt manager for 4 vCPUs
//! devices.init_interrupt_manager(4);
//!
//! // Add a custom device
//! let device_id = devices.try_add_mmio_dev(my_device)?;
//!
//! // Handle MMIO read
//! let value = devices.handle_mmio_read(gpa, AccessWidth::Bits32)?;
//!
//! // Handle MMIO write
//! devices.handle_mmio_write(gpa, AccessWidth::Bits32, 0xDEADBEEF)?;
//!
//! // Remove device (hot-unplug)
//! devices.remove_mmio_dev(device_id)?;
//! ```
//!
//! ### Device with Interrupt Support
//!
//! ```rust,ignore
//! use axdevice::{InterruptConfig, TriggerMode, CpuAffinity, IrqType};
//! use axdevice_base::{BaseDeviceOps, InterruptTrigger};
//! use core::cell::OnceCell;
//!
//! struct MyDevice {
//!     // ... device fields ...
//!     interrupt_trigger: OnceCell<Arc<dyn InterruptTrigger>>,
//! }
//!
//! impl BaseDeviceOps<GuestPhysAddrRange> for MyDevice {
//!     fn interrupt_config(&self) -> Option<InterruptConfig> {
//!         Some(InterruptConfig {
//!             primary_irq: 32,
//!             additional_irqs: vec![],
//!             trigger_mode: TriggerMode::Level,
//!             cpu_affinity: CpuAffinity::Fixed(0),
//!             priority: 100,
//!         })
//!     }
//!
//!     fn set_interrupt_trigger(&self, trigger: Arc<dyn InterruptTrigger>) {
//!         self.interrupt_trigger.set(trigger).ok();
//!     }
//!
//!     fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
//!         // ... device logic ...
//!
//!         // Trigger interrupt when needed
//!         if need_interrupt {
//!             if let Some(trigger) = self.interrupt_trigger.get() {
//!                 trigger.trigger(IrqType::Primary)?;
//!             }
//!         }
//!
//!         Ok(())
//!     }
//! }
//! ```
//!
//! ### Handling Interrupts in VM Loop
//!
//! ```rust,ignore
//! loop {
//!     // Pop pending interrupts before VM entry
//!     while let Some(irq) = devices.pop_pending_interrupt(cpu_id) {
//!         vcpu.inject_interrupt(irq.irq);
//!     }
//!
//!     // Run vCPU
//!     vcpu.run()?;
//!
//!     // Handle VM exits...
//! }
//! ```

extern crate alloc;
#[macro_use]
extern crate log;

mod config;
mod device;
mod device_cell;
mod lifecycle;
mod region;
mod wrapper;
mod registry;
mod notify;
pub mod virtio;

pub use config::AxVmDeviceConfig;
pub use device::AxVmDevices;
pub use device_cell::DeviceCell;
pub use lifecycle::{DeviceLifecycle, DeviceState};
pub use region::CachedRegions;
pub use wrapper::{DeviceId, DeviceStats, DeviceWrapper};
pub use registry::DeviceRegistry;

// Re-export notification management types
pub use notify::{
    DeviceNotificationManager,
    DeviceNotifierImpl,
    PendingNotification,
    PollFlags,
    // Legacy type aliases for backward compatibility
    DeviceInterruptManager,
    DeviceInterruptTrigger,
    PendingInterrupt,
};

// Re-export commonly used notification types from axdevice_base
pub use axdevice_base::{
    // New notification API
    DeviceEvent,
    DeviceNotifier,
    NotificationConfig,
    NotifyMethod,
    // Legacy interrupt API (deprecated)
    InterruptConfig,
    InterruptTrigger,
    IrqType,
    TriggerMode,
    CpuAffinity,
};

// Re-export backend types when virtio-blk feature is enabled
#[cfg(feature = "virtio-blk")]
pub use virtio::blk_backend::AxBlockBackend;

// Re-export console backend types when virtio-console feature is enabled
#[cfg(feature = "virtio-console")]
pub use virtio::console_backend::AxConsoleBackend;

/// Trait for console devices that can forward input to the guest.
///
/// This trait is implemented by VirtIO console devices and allows the hypervisor
/// to forward UART input to the guest when an interrupt fires.
pub trait ConsoleInputHandler: Send + Sync {
    /// Forward pending input from backend (UART) to the guest.
    ///
    /// This reads data from the backend and pushes it to the VirtIO receiveq,
    /// then triggers an interrupt to notify the guest.
    ///
    /// Returns the number of bytes forwarded to the guest.
    fn forward_input(&self) -> usize;
}

/// Global console handler for VirtIO console input.
///
/// This stores a reference to the console device so it can be called from
/// the vCPU loop when UART input is detected.
static GLOBAL_CONSOLE_HANDLER: spin::Once<alloc::sync::Arc<dyn ConsoleInputHandler>> = spin::Once::new();

/// Register the global console handler.
///
/// This should be called once when the VirtIO console device is created.
pub fn register_console_handler(handler: alloc::sync::Arc<dyn ConsoleInputHandler>) {
    GLOBAL_CONSOLE_HANDLER.call_once(|| handler);
}

/// Forward UART input to the guest via VirtIO console.
///
/// This should be called when UART input is detected (e.g., after UART IRQ)
/// to forward host input to the guest.
///
/// Returns the number of bytes forwarded to the guest.
pub fn forward_console_input() -> usize {
    if let Some(handler) = GLOBAL_CONSOLE_HANDLER.get() {
        handler.forward_input()
    } else {
        0
    }
}
