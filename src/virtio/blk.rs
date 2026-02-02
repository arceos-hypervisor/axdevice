//! VirtIO Block device adapter for axdevice framework.
//!
//! This module provides an adapter that wraps the `VirtioMmioBlockDevice` from
//! axvirtio-blk and implements the `BaseDeviceOps` trait from axdevice_base,
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
//! │         VirtioBlkDevice<B, T>               │
//! │            (This Adapter)                   │
//! │   - Implements BaseDeviceOps                │
//! │   - Wraps VirtioMmioBlockDevice             │
//! │   - Handles interrupt triggering            │
//! └─────────────────┬───────────────────────────┘
//!                   │
//!                   ▼
//! ┌─────────────────────────────────────────────┐
//! │     VirtioMmioBlockDevice<B, T>             │
//! │         (from axvirtio-blk)                 │
//! │   - VirtIO MMIO transport                   │
//! │   - Block request processing                │
//! └─────────────────┬───────────────────────────┘
//!                   │
//!                   ▼
//! ┌─────────────────────────────────────────────┐
//! │            BlockBackend                     │
//! │   (User-provided storage backend)           │
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use axdevice::virtio::VirtioBlkDevice;
//! use axvirtio_blk::BlockBackend;
//!
//! // Create a block backend (e.g., file-backed or memory-backed)
//! let backend = MyBlockBackend::new();
//!
//! // Create the VirtIO block device
//! let device = VirtioBlkDevice::new(
//!     base_addr,
//!     size,
//!     backend,
//!     block_config,
//!     memory_accessor,
//! )?;
//!
//! // Add to the VM's device list
//! vm_devices.try_add_mmio_dev(Arc::new(device))?;
//! ```

use alloc::sync::Arc;
use spin::RwLock;

use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr, GuestPhysAddrRange, device::AccessWidth};
use axdevice_base::{
    BaseDeviceOps, EmuDeviceType, InterruptConfig, InterruptTrigger, IrqType,
    CpuAffinity, TriggerMode,
};
use axerrno::AxResult;

use axvirtio_blk::{BlockBackend, VirtioMmioBlockDevice, VirtioBlockConfig};

/// VirtIO Block device adapter for axdevice framework.
///
/// This struct wraps the `VirtioMmioBlockDevice` from axvirtio-blk and implements
/// the `BaseDeviceOps` trait, enabling integration with the AxVisor device
/// management system.
///
/// # Type Parameters
///
/// * `B` - Block backend implementation that handles actual storage operations
/// * `T` - Guest memory accessor with address translation capabilities
///
/// # Interrupt Handling
///
/// The device supports interrupt triggering through the `InterruptTrigger` trait.
/// When the VirtIO device needs to signal the guest (e.g., after completing an I/O
/// request), it calls `trigger_interrupt()` which uses the injected trigger.
pub struct VirtioBlkDevice<B: BlockBackend, T: GuestMemoryAccessor + Clone + Send + Sync> {
    /// The underlying VirtIO MMIO block device.
    inner: VirtioMmioBlockDevice<B, T>,
    /// The address range of this device.
    range: GuestPhysAddrRange,
    /// Interrupt trigger injected by the device framework.
    /// Uses RwLock for thread-safe interior mutability.
    interrupt_trigger: RwLock<Option<Arc<dyn InterruptTrigger>>>,
    /// Interrupt configuration.
    irq_id: u32,
}

impl<B: BlockBackend + 'static, T: GuestMemoryAccessor + Clone + Send + Sync + 'static> VirtioBlkDevice<B, T> {
    /// Creates a new VirtIO Block device.
    ///
    /// # Arguments
    ///
    /// * `base_ipa` - Base guest physical address of the device's MMIO region
    /// * `length` - Size of the MMIO region in bytes
    /// * `backend` - Block backend for storage operations
    /// * `block_config` - VirtIO block device configuration
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
        block_config: VirtioBlockConfig,
        accessor: T,
        irq_id: u32,
    ) -> AxResult<Self> {
        let inner = VirtioMmioBlockDevice::new(
            base_ipa,
            length,
            backend,
            block_config,
            accessor,
        ).map_err(|_| axerrno::ax_err_type!(BadState, "Failed to create VirtIO block device"))?;

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

    /// Gets the device status.
    pub fn get_status(&self) -> u32 {
        self.inner.get_status()
    }

    /// Checks if the device is ready.
    pub fn is_device_ready(&self) -> bool {
        self.inner.is_device_ready()
    }

    /// Triggers an interrupt to the guest.
    ///
    /// This is called by the VirtIO device when it needs to notify the guest
    /// (e.g., after completing an I/O request).
    pub fn trigger_interrupt(&self) -> AxResult {
        if let Some(trigger) = self.interrupt_trigger.read().as_ref() {
            trigger.trigger(IrqType::Primary)
        } else {
            Ok(())
        }
    }
}

impl<B: BlockBackend + 'static, T: GuestMemoryAccessor + Clone + Send + Sync + 'static>
    BaseDeviceOps<GuestPhysAddrRange> for VirtioBlkDevice<B, T>
{
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::VirtioBlk
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
        let int_status = self.inner.get_interrupt_status();
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

/// Configuration builder for VirtIO Block device.
///
/// Provides a fluent interface for configuring and creating a VirtIO Block device.
///
/// # Example
///
/// ```rust,ignore
/// let device = VirtioBlkDeviceBuilder::new()
///     .base_address(0x0a000000.into())
///     .size(0x200)
///     .irq(32)
///     .capacity_sectors(1024 * 1024) // 512MB
///     .build(backend, accessor)?;
/// ```
pub struct VirtioBlkDeviceBuilder {
    base_ipa: Option<GuestPhysAddr>,
    length: usize,
    irq_id: u32,
    block_config: VirtioBlockConfig,
}

impl Default for VirtioBlkDeviceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioBlkDeviceBuilder {
    /// Creates a new builder with default values.
    pub fn new() -> Self {
        Self {
            base_ipa: None,
            length: 0x200, // 512 bytes default
            irq_id: 1,
            block_config: VirtioBlockConfig::default(),
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

    /// Sets the capacity in sectors (512 bytes each).
    pub fn capacity_sectors(mut self, sectors: u64) -> Self {
        self.block_config.capacity = sectors;
        self
    }

    /// Sets the capacity in bytes.
    pub fn capacity_bytes(mut self, bytes: u64) -> Self {
        self.block_config.capacity = bytes / 512;
        self
    }

    /// Sets the block size in bytes.
    pub fn block_size(mut self, size: u32) -> Self {
        self.block_config.blk_size = size;
        self
    }

    /// Sets the maximum segment size.
    pub fn max_segment_size(mut self, size: u32) -> Self {
        self.block_config.size_max = size;
        self
    }

    /// Sets the maximum number of segments per request.
    pub fn max_segments(mut self, count: u32) -> Self {
        self.block_config.seg_max = count;
        self
    }

    /// Builds the VirtIO Block device.
    ///
    /// # Arguments
    ///
    /// * `backend` - Block backend for storage operations
    /// * `accessor` - Guest memory accessor
    ///
    /// # Errors
    ///
    /// Returns an error if the base address is not set or device creation fails.
    pub fn build<B, T>(self, backend: B, accessor: T) -> AxResult<VirtioBlkDevice<B, T>>
    where
        B: BlockBackend + 'static,
        T: GuestMemoryAccessor + Clone + Send + Sync + 'static,
    {
        let base_ipa = self.base_ipa
            .ok_or_else(|| axerrno::ax_err_type!(InvalidInput, "Base address not set"))?;

        VirtioBlkDevice::new(
            base_ipa,
            self.length,
            backend,
            self.block_config,
            accessor,
            self.irq_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;
    use axvirtio_common::VirtioResult;
    use spin::Mutex;

    /// Mock block backend for testing.
    struct MockBlockBackend {
        data: Mutex<BTreeMap<u64, Vec<u8>>>,
    }

    impl MockBlockBackend {
        fn new() -> Self {
            Self {
                data: Mutex::new(BTreeMap::new()),
            }
        }
    }

    impl BlockBackend for MockBlockBackend {
        fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
            let data = self.data.lock();
            if let Some(sector_data) = data.get(&sector) {
                let len = buffer.len().min(sector_data.len());
                buffer[..len].copy_from_slice(&sector_data[..len]);
                Ok(len)
            } else {
                buffer.fill(0);
                Ok(buffer.len())
            }
        }

        fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
            let mut data = self.data.lock();
            data.insert(sector, buffer.to_vec());
            Ok(buffer.len())
        }

        fn flush(&self) -> VirtioResult<()> {
            Ok(())
        }
    }

    /// Mock guest memory accessor for testing.
    #[derive(Clone)]
    struct MockGuestMemoryAccessor {
        memory: Arc<Mutex<BTreeMap<usize, u8>>>,
    }

    impl MockGuestMemoryAccessor {
        fn new() -> Self {
            Self {
                memory: Arc::new(Mutex::new(BTreeMap::new())),
            }
        }
    }

    // Safety: MockGuestMemoryAccessor is safe to send and share between threads
    // because it uses Arc<Mutex<...>> internally.
    unsafe impl Send for MockGuestMemoryAccessor {}
    unsafe impl Sync for MockGuestMemoryAccessor {}

    impl GuestMemoryAccessor for MockGuestMemoryAccessor {
        fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(memory_addr::PhysAddr, usize)> {
            // For testing, we just return a mock physical address and large limit
            // In real implementation, this would do actual address translation
            Some((memory_addr::PhysAddr::from(guest_addr.as_usize()), 0x10000))
        }

        fn read_buffer(&self, guest_addr: GuestPhysAddr, buffer: &mut [u8]) -> Result<(), axerrno::AxError> {
            let memory = self.memory.lock();
            for (i, byte) in buffer.iter_mut().enumerate() {
                *byte = *memory.get(&(guest_addr.as_usize() + i)).unwrap_or(&0);
            }
            Ok(())
        }

        fn write_buffer(&self, guest_addr: GuestPhysAddr, buffer: &[u8]) -> Result<(), axerrno::AxError> {
            let mut memory = self.memory.lock();
            for (i, byte) in buffer.iter().enumerate() {
                memory.insert(guest_addr.as_usize() + i, *byte);
            }
            Ok(())
        }

        fn read_obj<O>(&self, addr: GuestPhysAddr) -> Result<O, axerrno::AxError> {
            let size = core::mem::size_of::<O>();
            let mut bytes = alloc::vec![0u8; size];
            self.read_buffer(addr, &mut bytes)?;
            // SAFETY: We've read exactly `size` bytes into a properly aligned buffer
            Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const O) })
        }

        fn write_obj<O>(&self, addr: GuestPhysAddr, obj: O) -> Result<(), axerrno::AxError> {
            let size = core::mem::size_of::<O>();
            // SAFETY: We're converting the object to bytes
            let bytes = unsafe {
                core::slice::from_raw_parts(&obj as *const O as *const u8, size)
            };
            self.write_buffer(addr, bytes)
        }
    }

    #[test]
    fn test_virtio_blk_device_creation() {
        let backend = MockBlockBackend::new();
        let accessor = MockGuestMemoryAccessor::new();

        let device = VirtioBlkDeviceBuilder::new()
            .base_address(0x0a000000.into())
            .size(0x200)
            .irq(32)
            .capacity_sectors(1024)
            .build(backend, accessor);

        assert!(device.is_ok());

        let device = device.unwrap();
        assert_eq!(device.emu_type(), EmuDeviceType::VirtioBlk);
        assert_eq!(device.address_ranges().len(), 1);
    }

    #[test]
    fn test_virtio_blk_device_mmio_read() {
        let backend = MockBlockBackend::new();
        let accessor = MockGuestMemoryAccessor::new();

        let device = VirtioBlkDeviceBuilder::new()
            .base_address(0x0a000000.into())
            .size(0x200)
            .irq(32)
            .build(backend, accessor)
            .unwrap();

        // Read magic value (0x74726976 = "virt")
        let result = device.handle_read(0x0a000000.into(), AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x74726976);

        // Read version (should be 2)
        let result = device.handle_read(0x0a000004.into(), AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);

        // Read device ID (should be 2 for block device)
        let result = device.handle_read(0x0a000008.into(), AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);
    }

    #[test]
    fn test_virtio_blk_device_interrupt_config() {
        let backend = MockBlockBackend::new();
        let accessor = MockGuestMemoryAccessor::new();

        let device = VirtioBlkDeviceBuilder::new()
            .base_address(0x0a000000.into())
            .irq(42)
            .build(backend, accessor)
            .unwrap();

        let config = device.interrupt_config();
        assert!(config.is_some());

        let config = config.unwrap();
        assert_eq!(config.primary_irq, 42);
        assert_eq!(config.trigger_mode, TriggerMode::Level);
    }
}
