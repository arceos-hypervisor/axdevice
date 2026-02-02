//! Device wrapper with lifecycle tracking, locking, statistics, and region caching.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use axaddrspace::device::{AccessWidth, DeviceAddrRange};
use axerrno::{ax_err, AxResult};
use axdevice_base::{BaseDeviceOps, RegionHit};

use crate::lifecycle::{DeviceLifecycle, DeviceState};
use crate::region::CachedRegions;

/// Unique identifier for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub usize);

impl DeviceId {
    /// Special device ID for passthrough devices (IRQ-based).
    /// The ID encodes the IRQ number: PASSTHROUGH_BASE + irq
    pub const PASSTHROUGH_BASE: usize = 0x1000_0000;

    /// Creates a device ID for a passthrough device with the given IRQ.
    #[inline]
    pub const fn passthrough(irq: u32) -> Self {
        Self(Self::PASSTHROUGH_BASE + irq as usize)
    }

    /// Checks if this device ID represents a passthrough device.
    #[inline]
    pub const fn is_passthrough(&self) -> bool {
        self.0 >= Self::PASSTHROUGH_BASE
    }
}

/// Statistics for device access operations.
#[derive(Debug, Default)]
pub struct DeviceStats {
    /// Total number of read operations.
    pub read_count: AtomicU64,
    /// Total number of write operations.
    pub write_count: AtomicU64,
    /// Total number of failed operations.
    pub error_count: AtomicU64,
}

impl DeviceStats {
    /// Creates a new statistics tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a read operation.
    #[inline]
    pub fn record_read(&self) {
        self.read_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a write operation.
    #[inline]
    pub fn record_write(&self) {
        self.write_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an error.
    #[inline]
    pub fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Gets the total number of reads.
    #[inline]
    pub fn reads(&self) -> u64 {
        self.read_count.load(Ordering::Relaxed)
    }

    /// Gets the total number of writes.
    #[inline]
    pub fn writes(&self) -> u64 {
        self.write_count.load(Ordering::Relaxed)
    }

    /// Gets the total number of errors.
    #[inline]
    pub fn errors(&self) -> u64 {
        self.error_count.load(Ordering::Relaxed)
    }

    /// Gets the total number of operations (reads + writes).
    #[inline]
    pub fn total_operations(&self) -> u64 {
        self.reads() + self.writes()
    }
}

/// Wrapper around a device that adds lifecycle management, locking, statistics, and region caching.
///
/// This structure provides:
/// - **Lifecycle management**: State tracking (Active/Removing/Removed)
/// - **Concurrency control**: Per-device mutex for exclusive access
/// - **Statistics**: Operation counters for monitoring
/// - **Device identity**: Unique ID for lookup
/// - **Region caching**: Zero-allocation region lookups for multi-region devices
pub struct DeviceWrapper<R: DeviceAddrRange> {
    /// The wrapped device implementation.
    inner: Arc<dyn BaseDeviceOps<R>>,
    /// Lifecycle state and access tracking.
    lifecycle: Arc<DeviceLifecycle>,
    /// Per-device lock for exclusive access.
    lock: Arc<Mutex<()>>,
    /// Unique device identifier.
    id: DeviceId,
    /// Access statistics.
    stats: DeviceStats,
    /// Cached region information for zero-allocation lookups.
    /// `None` if the device doesn't provide region information.
    cached_regions: Option<CachedRegions>,
}

impl<R: DeviceAddrRange + 'static> DeviceWrapper<R> {
    /// Creates a new device wrapper.
    ///
    /// This also caches the device's region information if available,
    /// enabling zero-allocation region lookups during MMIO handling.
    pub fn new(id: DeviceId, device: Arc<dyn BaseDeviceOps<R>>) -> Self {
        // Cache region info at construction time (called once)
        let cached_regions = device
            .region_descriptor()
            .map(|desc| CachedRegions::from_descriptor(&desc));

        Self {
            inner: device,
            lifecycle: Arc::new(DeviceLifecycle::new()),
            lock: Arc::new(Mutex::new(())),
            id,
            stats: DeviceStats::new(),
            cached_regions,
        }
    }

    /// Gets the device ID.
    #[inline]
    pub fn id(&self) -> DeviceId {
        self.id
    }

    /// Gets the device's address ranges.
    #[inline]
    pub fn address_ranges(&self) -> &[R] {
        self.inner.address_ranges()
    }

    /// Gets a reference to the lifecycle tracker.
    #[inline]
    pub fn lifecycle(&self) -> &Arc<DeviceLifecycle> {
        &self.lifecycle
    }

    /// Gets a reference to the statistics.
    #[inline]
    pub fn stats(&self) -> &DeviceStats {
        &self.stats
    }

    /// Gets a reference to the inner device.
    #[inline]
    pub fn inner(&self) -> &Arc<dyn BaseDeviceOps<R>> {
        &self.inner
    }

    /// Looks up a region by address (zero-allocation hot path).
    ///
    /// This method tries multiple strategies for region lookup:
    /// 1. Device's custom `region_lookup()` method (fastest, may be inlined)
    /// 2. Cached regions from `region_descriptor()` (cached at construction)
    ///
    /// # Arguments
    ///
    /// * `addr` - The address to look up.
    ///
    /// # Returns
    ///
    /// `Some(RegionHit)` if the address falls within a device region,
    /// `None` if the device doesn't have multi-region support or address not found.
    ///
    /// # Performance
    ///
    /// For devices with fixed layouts that implement `region_lookup()` with
    /// `#[inline(always)]`, this can be as fast as ~5ns per lookup.
    #[inline]
    pub fn lookup_region(&self, addr: usize) -> Option<RegionHit> {
        // First try device's custom implementation (may be optimized/inlined)
        if let Some(hit) = self.inner.region_lookup(addr) {
            return Some(hit);
        }

        // Fall back to cached regions
        self.cached_regions.as_ref()?.lookup(addr)
    }

    /// Returns whether this device has multi-region support.
    #[inline]
    pub fn has_regions(&self) -> bool {
        self.cached_regions.is_some()
    }

    /// Gets a reference to the cached regions (if any).
    #[inline]
    pub fn cached_regions(&self) -> Option<&CachedRegions> {
        self.cached_regions.as_ref()
    }

    /// Attempts to acquire access to the device for a read operation.
    ///
    /// Returns an `AccessGuard` if successful, or an error if the device
    /// is being removed or has been removed.
    pub fn try_access_for_read(&self, addr: R::Addr, width: AccessWidth) -> AxResult<usize> {
        // Try to begin access
        if !self.lifecycle.try_begin_access() {
            self.stats.record_error();
            return ax_err!(BadState, "Device is not active");
        }

        // Acquire device lock
        let _guard = self.lock.lock();

        // Perform read operation
        let result = self.inner.handle_read(addr, width);

        // Record statistics
        match &result {
            Ok(_) => self.stats.record_read(),
            Err(_) => self.stats.record_error(),
        }

        // End access (drop happens automatically)
        self.lifecycle.end_access();

        result
    }

    /// Attempts to acquire access to the device for a write operation.
    ///
    /// Returns `Ok(())` if successful, or an error if the device
    /// is being removed or has been removed.
    pub fn try_access_for_write(
        &self,
        addr: R::Addr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        // Try to begin access
        if !self.lifecycle.try_begin_access() {
            self.stats.record_error();
            return ax_err!(BadState, "Device is not active");
        }

        // Acquire device lock
        let _guard = self.lock.lock();

        // Perform write operation
        let result = self.inner.handle_write(addr, width, val);

        // Record statistics
        match &result {
            Ok(_) => self.stats.record_write(),
            Err(_) => self.stats.record_error(),
        }

        // End access
        self.lifecycle.end_access();

        result
    }

    /// Begins the removal process for this device.
    ///
    /// Returns `true` if removal started successfully, `false` if already removing/removed.
    pub fn begin_removal(&self) -> bool {
        self.lifecycle.begin_removal()
    }

    /// Waits for all active accesses to complete.
    pub fn wait_idle(&self) {
        self.lifecycle.wait_idle();
    }

    /// Completes the removal process.
    pub fn complete_removal(&self) {
        self.lifecycle.complete_removal();
    }

    /// Checks if the device is in Active state.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.lifecycle.state() == DeviceState::Active
    }

    /// Checks if the device is being removed.
    #[inline]
    pub fn is_removing(&self) -> bool {
        self.lifecycle.state() == DeviceState::Removing
    }

    /// Checks if the device has been removed.
    #[inline]
    pub fn is_removed(&self) -> bool {
        self.lifecycle.state() == DeviceState::Removed
    }
}

impl<R: DeviceAddrRange> Clone for DeviceWrapper<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            lifecycle: Arc::clone(&self.lifecycle),
            lock: Arc::clone(&self.lock),
            id: self.id,
            // Note: Stats are shared through Arc, so clones see the same counters
            stats: DeviceStats::new(), // Each wrapper gets its own stats
            // Cached regions are shared (read-only after construction)
            cached_regions: self.cached_regions.clone(),
        }
    }
}

impl<R: DeviceAddrRange> core::fmt::Debug for DeviceWrapper<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceWrapper")
            .field("id", &self.id)
            .field("lifecycle", &self.lifecycle)
            .field("stats", &format_args!(
                "reads={}, writes={}, errors={}",
                self.stats.reads(),
                self.stats.writes(),
                self.stats.errors()
            ))
            .field("has_regions", &self.cached_regions.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange};
    use axdevice_base::{BaseMmioDeviceOps, EmuDeviceType};

    struct MockDevice {
        ranges: [GuestPhysAddrRange; 1],
    }

    impl BaseDeviceOps<GuestPhysAddrRange> for MockDevice {
        fn emu_type(&self) -> EmuDeviceType {
            EmuDeviceType::Console
        }

        fn address_ranges(&self) -> &[GuestPhysAddrRange] {
            &self.ranges
        }

        fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
            Ok(0x42)
        }

        fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> AxResult {
            Ok(())
        }
    }

    #[test]
    fn test_wrapper_basic_access() {
        let device = Arc::new(MockDevice {
            ranges: [GuestPhysAddrRange::from_start_size(0x1000.into(), 0x100)],
        });
        let wrapper = DeviceWrapper::new(DeviceId(1), device);

        // Test read
        let result = wrapper.try_access_for_read(GuestPhysAddr::from(0x1000), AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x42);
        assert_eq!(wrapper.stats().reads(), 1);

        // Test write
        let result = wrapper.try_access_for_write(GuestPhysAddr::from(0x1000), AccessWidth::Dword, 100);
        assert!(result.is_ok());
        assert_eq!(wrapper.stats().writes(), 1);
    }

    #[test]
    fn test_wrapper_removal() {
        let device = Arc::new(MockDevice {
            ranges: [GuestPhysAddrRange::from_start_size(0x1000.into(), 0x100)],
        });
        let wrapper = DeviceWrapper::new(DeviceId(1), device);

        assert!(wrapper.is_active());

        // Begin removal
        assert!(wrapper.begin_removal());
        assert!(wrapper.is_removing());

        // Access should be denied
        let result = wrapper.try_access_for_read(GuestPhysAddr::from(0x1000), AccessWidth::Dword);
        assert!(result.is_err());
        assert_eq!(wrapper.stats().errors(), 1);

        // Complete removal
        wrapper.wait_idle();
        wrapper.complete_removal();
        assert!(wrapper.is_removed());
    }
}
