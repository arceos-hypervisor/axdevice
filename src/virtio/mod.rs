//! VirtIO device implementations.
//!
//! This module provides VirtIO device implementations for the axdevice framework:
//!
//! ## Implemented Devices
//!
//! - **VirtIO Block** (`virtio-blk` feature): Full VirtIO block device implementation
//!   wrapping the axvirtio-blk crate.
//!
//! - **VirtIO Console** (`virtio-console` feature): Full VirtIO console device implementation
//!   wrapping the axvirtio-console crate.
//!
//! - **Dummy Devices**: Minimal implementations that handle MMIO accesses without
//!   performing actual device operations. Used to prevent VM crashes when the guest
//!   OS tries to access devices that are not yet fully implemented.
//!
//! ## Feature Flags
//!
//! - `virtio-blk`: Enables the full VirtIO block device implementation.
//! - `virtio-console`: Enables the full VirtIO console device implementation.
//!
//! ## Example
//!
//! ```rust,ignore
//! // Using the full VirtIO block device (requires virtio-blk feature)
//! #[cfg(feature = "virtio-blk")]
//! {
//!     use axdevice::virtio::{VirtioBlkDevice, VirtioBlkDeviceBuilder};
//!
//!     let device = VirtioBlkDeviceBuilder::new()
//!         .base_address(0x0a000000.into())
//!         .size(0x200)
//!         .irq(32)
//!         .capacity_bytes(512 * 1024 * 1024) // 512MB
//!         .build(backend, accessor)?;
//! }
//!
//! // Using the full VirtIO console device (requires virtio-console feature)
//! #[cfg(feature = "virtio-console")]
//! {
//!     use axdevice::virtio::{VirtioConsoleDevice, VirtioConsoleDeviceBuilder};
//!
//!     let device = VirtioConsoleDeviceBuilder::new()
//!         .base_address(0x0a001000.into())
//!         .size(0x200)
//!         .irq(33)
//!         .terminal_size(80, 25)
//!         .build(backend, accessor)?;
//! }
//!
//! // Using dummy devices (always available)
//! {
//!     use axdevice::virtio::new_dummy_virtio_device;
//!
//!     let device = new_dummy_virtio_device(0x0a000000.into(), Some(0x200));
//! }
//! ```

#[cfg(feature = "virtio-blk")]
mod blk;

#[cfg(feature = "virtio-console")]
mod console;

#[cfg(feature = "virtio-blk")]
pub use blk::{VirtioBlkDevice, VirtioBlkDeviceBuilder};

#[cfg(feature = "virtio-blk")]
pub use axvirtio_blk::{BlockBackend, VirtioBlockConfig};

#[cfg(feature = "virtio-console")]
pub use console::{VirtioConsoleDevice, VirtioConsoleDeviceBuilder};

#[cfg(feature = "virtio-console")]
pub use axvirtio_console::{ConsoleBackend, VirtioConsoleConfig};

#[cfg(feature = "virtio-blk")]
pub mod blk_backend;

#[cfg(feature = "virtio-console")]
pub mod console_backend;

// Dummy device implementations (always available)

use alloc::sync::Arc;

use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, device::AccessWidth};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axerrno::{AxResult, AxError};

use crate::DeviceCell;

/// A dummy virtio device that handles MMIO accesses without actual functionality.
///
/// This device responds to virtio MMIO reads/writes with default values
/// to prevent VM crashes when the guest tries to access unimplemented virtio devices.
///
/// # Concurrency Safety
///
/// This device uses `DeviceCell` for interior mutability. Safety is guaranteed by
/// the framework's per-device lock in `DeviceWrapper`, which ensures exclusive access
/// during `handle_read/write` calls.
pub struct DummyVirtioDevice {
    /// The base address range of this device.
    range: GuestPhysAddrRange,
    /// Device state for tracking initialization.
    /// Uses DeviceCell for zero-cost interior mutability (framework guarantees exclusive access).
    state: DeviceCell<DummyVirtioState>,
}

/// Internal state for the dummy virtio device.
struct DummyVirtioState {
    /// Magic value (VIRTIO_MMIO_MAGIC_VALUE = 0x7472_6976)
    magic: u32,
    /// Version (VIRTIO_MMIO_VERSION = 2)
    version: u32,
    /// Device ID (0 = VirtioDeviceID::Reserved)
    device_id: u32,
    /// Vendor ID (0 = dummy vendor)
    vendor: u32,
    /// Device features
    features: u64,
    /// Driver features
    driver_features: u64,
    /// Queue selector
    queue_sel: u32,
    /// Queue size
    queue_size: u32,
    /// Queue ready
    queue_ready: u32,
    /// Queue notification
    queue_notification: u32,
    /// Device status
    status: u32,
}

impl DummyVirtioState {
    /// Create a new dummy virtio state with default values.
    const fn new() -> Self {
        Self {
            magic: 0x7472_6976, // "virt" in little endian
            version: 2,
            device_id: 0, // Reserved/Invalid
            vendor: 0,
            features: 0,
            driver_features: 0,
            queue_sel: 0,
            queue_size: 0,
            queue_ready: 0,
            queue_notification: 0,
            status: 0,
        }
    }
}

impl DummyVirtioDevice {
    /// Create a new dummy virtio device.
    ///
    /// # Arguments
    ///
    /// * `base` - The base guest physical address of the device.
    /// * `size` - Optional size of the device's MMIO region.
    pub fn new(base: GuestPhysAddr, size: Option<usize>) -> Self {
        let size = size.unwrap_or(0x1000); // Default 4KB
        let end = base + size;
        let range = (base.as_usize()..end.as_usize())
            .try_into()
            .expect("Invalid virtio device range");

        Self {
            range,
            state: DeviceCell::new(DummyVirtioState::new()),
        }
    }

    /// Handle read from virtio MMIO registers.
    fn handle_virtio_read(&self, offset: u64, width: AccessWidth) -> AxResult<usize> {
        let state = self.state.get();

        // VirtIO MMIO register offsets (from virtio spec)
        match offset {
            0x000 => match width {
                AccessWidth::Dword => Ok(state.magic as usize),
                _ => Err(AxError::BadAddress),
            },
            0x004 => match width {
                AccessWidth::Dword => Ok(state.version as usize),
                _ => Err(AxError::BadAddress),
            },
            0x008 => match width {
                AccessWidth::Dword => Ok(state.device_id as usize),
                _ => Err(AxError::BadAddress),
            },
            0x00c => match width {
                AccessWidth::Dword => Ok(state.vendor as usize),
                _ => Err(AxError::BadAddress),
            },
            0x010 => match width {
                AccessWidth::Dword => Ok((state.features & 0xFFFFFFFF) as usize),
                _ => Err(AxError::BadAddress),
            },
            0x014 => match width {
                AccessWidth::Dword => Ok((state.features >> 32) as usize),
                _ => Err(AxError::BadAddress),
            },
            0x020 => match width {
                AccessWidth::Dword => Ok((state.driver_features & 0xFFFFFFFF) as usize),
                _ => Err(AxError::BadAddress),
            },
            0x024 => match width {
                AccessWidth::Dword => Ok((state.driver_features >> 32) as usize),
                _ => Err(AxError::BadAddress),
            },
            0x030 => match width {
                AccessWidth::Dword => Ok(state.queue_sel as usize),
                _ => Err(AxError::BadAddress),
            },
            0x034 => match width {
                AccessWidth::Dword => Ok(state.queue_size as usize),
                _ => Err(AxError::BadAddress),
            },
            0x044 => match width {
                AccessWidth::Dword => Ok(state.queue_ready as usize),
                _ => Err(AxError::BadAddress),
            },
            0x050 => match width {
                AccessWidth::Dword => Ok(state.queue_notification as usize),
                _ => Err(AxError::BadAddress),
            },
            0x070 => match width {
                AccessWidth::Dword => Ok(state.status as usize),
                _ => Err(AxError::BadAddress),
            },
            // For config generation and other registers, return 0
            _ => match width {
                AccessWidth::Dword => Ok(0),
                AccessWidth::Qword => Ok(0),
                _ => Err(AxError::BadAddress),
            },
        }
    }

    /// Handle write to virtio MMIO registers.
    fn handle_virtio_write(&self, offset: u64, width: AccessWidth, val: usize) -> AxResult {
        let state = self.state.get_mut();

        match offset {
            0x020 => match width {
                AccessWidth::Dword => {
                    state.driver_features = (state.driver_features & 0xFFFFFFFF00000000)
                        | (val as u64 & 0xFFFFFFFF);
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            0x024 => match width {
                AccessWidth::Dword => {
                    state.driver_features = (state.driver_features & 0xFFFFFFFF)
                        | ((val as u64 & 0xFFFFFFFF) << 32);
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            0x030 => match width {
                AccessWidth::Dword => {
                    state.queue_sel = val as u32;
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            0x038 => match width {
                AccessWidth::Dword => {
                    // Queue ready - ignore
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            0x044 => match width {
                AccessWidth::Dword => {
                    state.queue_ready = val as u32;
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            0x050 => match width {
                AccessWidth::Dword => {
                    state.queue_notification = val as u32;
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            0x070 => match width {
                AccessWidth::Dword => {
                    state.status = val as u32;
                    Ok(())
                }
                _ => Err(AxError::BadAddress),
            },
            // Ignore writes to other registers
            _ => Ok(()),
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for DummyVirtioDevice {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::VirtioBlk // Default to VirtioBlk, can be configured later
    }

    fn address_ranges(&self) -> &[GuestPhysAddrRange] {
        core::slice::from_ref(&self.range)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        let base = self.range.start;
        let offset = addr.as_usize() as u64 - base.as_usize() as u64;
        self.handle_virtio_read(offset, width)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        let base = self.range.start;
        let offset = addr.as_usize() as u64 - base.as_usize() as u64;
        self.handle_virtio_write(offset, width, val)
    }
}

/// Create a new dummy virtio device wrapped in an Arc.
///
/// This is a convenience function for creating dummy virtio devices
/// that can be added to the VM's device list.
pub fn new_dummy_virtio_device(base: GuestPhysAddr, size: Option<usize>) -> Arc<dyn BaseDeviceOps<GuestPhysAddrRange>> {
    Arc::new(DummyVirtioDevice::new(base, size))
}

/// A dummy UART device that handles MMIO accesses without actual functionality.
///
/// This device responds to UART MMIO reads/writes with default values
/// to prevent VM crashes when the guest tries to access unimplemented UART devices.
///
/// # Concurrency Safety
///
/// This device uses `DeviceCell` for interior mutability. Safety is guaranteed by
/// the framework's per-device lock in `DeviceWrapper`, which ensures exclusive access
/// during `handle_read/write` calls.
pub struct DummyUartDevice {
    /// The base address range of this device.
    range: GuestPhysAddrRange,
    /// UART register state.
    /// Uses DeviceCell for zero-cost interior mutability (framework guarantees exclusive access).
    state: DeviceCell<DummyUartState>,
}

/// Internal state for the dummy UART device (16550 compatible).
struct DummyUartState {
    /// Receiver hold register (RHR / transmit hold register THR)
    rhr_thr: u8,
    /// Interrupt enable register (IER)
    ier: u8,
    /// FIFO control register (FCR)
    fcr: u8,
    /// Line control register (LCR)
    lcr: u8,
    /// Line status register (LSR)
    lsr: u8,
}

impl DummyUartState {
    const fn new() -> Self {
        Self {
            rhr_thr: 0,
            ier: 0,
            fcr: 0,
            lcr: 0,
            lsr: 0x60, // LSR: THRE + TEMT (transmitter empty)
        }
    }
}

impl DummyUartDevice {
    /// Create a new dummy UART device.
    ///
    /// # Arguments
    ///
    /// * `base` - The base guest physical address of the device.
    /// * `size` - Optional size of the device's MMIO region.
    pub fn new(base: GuestPhysAddr, size: Option<usize>) -> Self {
        let size = size.unwrap_or(0x1000); // Default 4KB
        let end = base + size;
        let range = (base.as_usize()..end.as_usize())
            .try_into()
            .expect("Invalid UART device range");

        Self {
            range,
            state: DeviceCell::new(DummyUartState::new()),
        }
    }

    /// Handle read from UART MMIO registers (16550 compatible).
    fn handle_uart_read(&self, offset: u64, _width: AccessWidth) -> AxResult<usize> {
        let state = self.state.get();

        let val = match offset {
            // RBR/THR (Receiver Buffer Register / Transmit Holding Register)
            0x0 => state.rhr_thr as usize,
            // IER (Interrupt Enable Register)
            0x1 => state.ier as usize,
            // IIR (Interrupt Identification Register) - read-only
            // Return 0x01 = no interrupt pending. Returning 0 would indicate
            // a modem status interrupt pending, causing driver confusion.
            0x2 => 0x01,
            // LCR (Line Control Register)
            0x3 => state.lcr as usize,
            // MCR (Modem Control Register) - offset 0x4
            0x4 => 0x08, // OUT2 set (enables IRQ)
            // LSR (Line Status Register)
            // Always show transmitter empty + holding register empty
            0x5 => state.lsr as usize,
            // MSR (Modem Status Register)
            // Return DCD|DSR|CTS set to prevent Linux tty layer from blocking
            // on open() waiting for carrier detect (~30s timeout otherwise).
            0x6 => 0xB0, // DCD + DSR + CTS
            // For other registers, return 0
            _ => 0,
        };

        Ok(val)
    }

    /// Handle write to UART MMIO registers (16550 compatible).
    fn handle_uart_write(&self, offset: u64, _width: AccessWidth, val: usize) -> AxResult {
        let state = self.state.get_mut();

        match offset {
            // THR (Transmit Holding Register) - silently consume output
            0x0 => { state.rhr_thr = val as u8; }
            // IER (Interrupt Enable Register)
            0x1 => { state.ier = val as u8; }
            // FCR (FIFO Control Register)
            0x2 => { state.fcr = val as u8; }
            // LCR (Line Control Register)
            0x3 => { state.lcr = val as u8; }
            // Ignore writes to other registers
            _ => {}
        }
        Ok(())
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for DummyUartDevice {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_ranges(&self) -> &[GuestPhysAddrRange] {
        core::slice::from_ref(&self.range)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        let base = self.range.start;
        let offset = addr.as_usize() as u64 - base.as_usize() as u64;
        self.handle_uart_read(offset, width)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        let base = self.range.start;
        let offset = addr.as_usize() as u64 - base.as_usize() as u64;
        self.handle_uart_write(offset, width, val)
    }
}

/// Create a new dummy UART device wrapped in an Arc.
///
/// This is a convenience function for creating dummy UART devices
/// that can be added to the VM's device list.
pub fn new_dummy_uart_device(base: GuestPhysAddr, size: Option<usize>) -> Arc<dyn BaseDeviceOps<GuestPhysAddrRange>> {
    Arc::new(DummyUartDevice::new(base, size))
}

/// A dummy CLINT device that handles MMIO accesses without actual functionality.
///
/// This device responds to CLINT MMIO reads/writes with default values
/// to prevent VM crashes when the guest tries to access unimplemented CLINT devices.
pub struct DummyClintDevice {
    /// The base address range of this device.
    range: GuestPhysAddrRange,
}

impl DummyClintDevice {
    /// Create a new dummy CLINT device.
    ///
    /// # Arguments
    ///
    /// * `base` - The base guest physical address of the device.
    /// * `size` - Optional size of the device's MMIO region.
    pub fn new(base: GuestPhysAddr, size: Option<usize>) -> Self {
        let size = size.unwrap_or(0x10000); // Default 64KB
        let end = base + size;
        let range = (base.as_usize()..end.as_usize())
            .try_into()
            .expect("Invalid CLINT device range");

        Self { range }
    }

    /// Handle read from CLINT MMIO registers.
    fn handle_clint_read(&self, _offset: u64, width: AccessWidth) -> AxResult<usize> {
        // Return 0 for all CLINT registers
        match width {
            AccessWidth::Byte => Ok(0),
            AccessWidth::Word => Ok(0),
            AccessWidth::Dword => Ok(0),
            AccessWidth::Qword => Ok(0),
        }
    }

    /// Handle write to CLINT MMIO registers.
    fn handle_clint_write(&self, _offset: u64, _width: AccessWidth, _val: usize) -> AxResult {
        // Ignore all writes to CLINT registers
        Ok(())
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for DummyClintDevice {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy
    }

    fn address_ranges(&self) -> &[GuestPhysAddrRange] {
        core::slice::from_ref(&self.range)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        let base = self.range.start;
        let offset = addr.as_usize() as u64 - base.as_usize() as u64;
        self.handle_clint_read(offset, width)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        let base = self.range.start;
        let offset = addr.as_usize() as u64 - base.as_usize() as u64;
        self.handle_clint_write(offset, width, val)
    }
}

/// Create a new dummy CLINT device wrapped in an Arc.
///
/// This is a convenience function for creating dummy CLINT devices
/// that can be added to the VM's device list.
pub fn new_dummy_clint_device(base: GuestPhysAddr, size: Option<usize>) -> Arc<dyn BaseDeviceOps<GuestPhysAddrRange>> {
    Arc::new(DummyClintDevice::new(base, size))
}
