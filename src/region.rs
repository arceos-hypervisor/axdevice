//! Region caching for zero-allocation lookup in hot paths.
//!
//! This module provides the `CachedRegions` struct which caches device region
//! information at registration time, enabling fast, zero-allocation lookups
//! during MMIO operations.
//!
//! # Performance
//!
//! The caching approach provides significant performance benefits:
//! - **Without caching**: Each MMIO access calls `region_descriptor()` which may
//!   allocate memory or perform expensive computations
//! - **With caching**: Region info is computed once at registration, lookups are
//!   O(n) where n ≤ 8 (max regions per device)
//!
//! # Usage
//!
//! ```rust,ignore
//! // At registration time
//! let cached = CachedRegions::from_device(&device);
//!
//! // During MMIO handling (hot path)
//! if let Some(hit) = cached.lookup(addr) {
//!     // Zero-allocation lookup!
//!     match hit.region_id {
//!         RegionId::CONTROL => device.handle_control(hit.offset, width),
//!         RegionId::DATA => device.handle_data(hit.offset, width),
//!         _ => ...
//!     }
//! }
//! ```

use core::sync::atomic::{AtomicU32, Ordering};

use arrayvec::ArrayVec;
use axdevice_base::{
    DeviceRegion, RegionDescriptor, RegionHit, MAX_REGIONS_PER_DEVICE,
};

/// Cached region information for a device.
///
/// This struct caches the region information from a device's `region_descriptor()`
/// method at registration time. The cache is read-only after initialization,
/// making it safe for concurrent access without locks.
///
/// For devices that support dynamic region changes (e.g., PCI BAR remapping),
/// use `notify_change()` to increment the version and trigger a re-read.
pub struct CachedRegions {
    /// Cached region descriptors.
    regions: ArrayVec<DeviceRegion, MAX_REGIONS_PER_DEVICE>,
    /// Version number for cache invalidation.
    /// Incremented when regions change (e.g., PCI BAR remapping).
    version: AtomicU32,
}

impl Clone for CachedRegions {
    fn clone(&self) -> Self {
        let mut regions: ArrayVec<DeviceRegion, MAX_REGIONS_PER_DEVICE> = ArrayVec::new();
        for region in &self.regions {
            regions.push(region.clone());
        }
        Self {
            regions,
            version: AtomicU32::new(self.version.load(Ordering::Acquire)),
        }
    }
}

impl CachedRegions {
    /// Creates an empty cached regions container.
    pub fn new() -> Self {
        Self {
            regions: ArrayVec::new(),
            version: AtomicU32::new(0),
        }
    }

    /// Creates a cached regions container from a region descriptor.
    pub fn from_descriptor(desc: &RegionDescriptor) -> Self {
        let mut regions = ArrayVec::new();
        for region in desc.regions() {
            regions.push(region.clone());
        }
        Self {
            regions,
            version: AtomicU32::new(0),
        }
    }

    /// Creates cached regions from a device that implements `region_descriptor()`.
    ///
    /// Returns `None` if the device doesn't provide region information.
    #[inline]
    pub fn from_device<R, D>(device: &D) -> Option<Self>
    where
        D: axdevice_base::BaseDeviceOps<R>,
        R: axaddrspace::device::DeviceAddrRange,
    {
        device.region_descriptor().map(|desc| Self::from_descriptor(&desc))
    }

    /// Looks up an address in the cached regions (zero-allocation).
    ///
    /// This is the hot-path method used during MMIO handling. It returns
    /// a `RegionHit` on the stack without any heap allocation.
    ///
    /// # Arguments
    ///
    /// * `addr` - The address to look up.
    ///
    /// # Returns
    ///
    /// `Some(RegionHit)` if the address falls within a cached region,
    /// `None` otherwise.
    #[inline]
    pub fn lookup(&self, addr: usize) -> Option<RegionHit> {
        // Linear search is fine for n ≤ 8
        self.regions.iter().find_map(|r: &DeviceRegion| r.try_hit(addr))
    }

    /// Gets the number of cached regions.
    #[inline]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Returns true if there are no cached regions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Gets the current cache version.
    ///
    /// This can be used to detect if regions have changed since the last access.
    #[inline]
    pub fn version(&self) -> u32 {
        self.version.load(Ordering::Acquire)
    }

    /// Increments the version number and updates the regions.
    ///
    /// This is called when a device's regions change dynamically
    /// (e.g., PCI BAR remapping).
    pub fn update(&mut self, desc: &RegionDescriptor) {
        self.regions.clear();
        for region in desc.regions() {
            self.regions.push(region.clone());
        }
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    /// Gets the cached regions slice.
    #[inline]
    pub fn regions(&self) -> &[DeviceRegion] {
        &self.regions
    }
}

impl Default for CachedRegions {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for CachedRegions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CachedRegions")
            .field("regions", &self.regions.len())
            .field("version", &self.version())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axdevice_base::{Permissions, RegionId, RegionType};

    #[test]
    fn test_cached_regions_basic() {
        let desc = RegionDescriptor::new()
            .with_region(
                DeviceRegion::new(RegionId::CONTROL, "control", 0x1000, 0x100)
                    .with_type(RegionType::Control),
            )
            .with_region(
                DeviceRegion::new(RegionId::DATA, "data", 0x2000, 0x1000)
                    .with_type(RegionType::Data)
                    .with_permissions(Permissions::ReadOnly),
            );

        let cached = CachedRegions::from_descriptor(&desc);
        assert_eq!(cached.len(), 2);

        // Test lookup in control region
        let hit = cached.lookup(0x1050).unwrap();
        assert_eq!(hit.region_id, RegionId::CONTROL);
        assert_eq!(hit.offset, 0x50);
        assert_eq!(hit.region_type, RegionType::Control);

        // Test lookup in data region
        let hit = cached.lookup(0x2500).unwrap();
        assert_eq!(hit.region_id, RegionId::DATA);
        assert_eq!(hit.offset, 0x500);
        assert_eq!(hit.permissions, Permissions::ReadOnly);

        // Test lookup miss
        assert!(cached.lookup(0x500).is_none());
        assert!(cached.lookup(0x3000).is_none());
    }

    #[test]
    fn test_cached_regions_update() {
        let desc1 = RegionDescriptor::new()
            .with_region(DeviceRegion::new(RegionId::CONTROL, "control", 0x1000, 0x100));

        let mut cached = CachedRegions::from_descriptor(&desc1);
        assert_eq!(cached.len(), 1);
        assert_eq!(cached.version(), 0);

        // Update with new descriptor
        let desc2 = RegionDescriptor::new()
            .with_region(DeviceRegion::new(RegionId::CONTROL, "control", 0x2000, 0x100))
            .with_region(DeviceRegion::new(RegionId::DATA, "data", 0x3000, 0x200));

        cached.update(&desc2);
        assert_eq!(cached.len(), 2);
        assert_eq!(cached.version(), 1);

        // Old address should no longer hit
        assert!(cached.lookup(0x1050).is_none());

        // New address should hit
        let hit = cached.lookup(0x2050).unwrap();
        assert_eq!(hit.region_id, RegionId::CONTROL);
    }
}
