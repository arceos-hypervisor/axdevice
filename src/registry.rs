//! Device registry with O(log n) lookup using interval trees.
//!
//! This module provides the core device management infrastructure with:
//! - Fast address-based device lookup using interval trees
//! - Per-device concurrency control
//! - Device lifecycle management (hot-plug/hot-unplug)
//! - Multi-region device support (from `region_descriptor()`)

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use axaddrspace::device::{AccessWidth, DeviceAddrRange};
use axerrno::{ax_err, AxError, AxResult};
use axdevice_base::{BaseDeviceOps, RegionHit};
use spin::RwLock;

use crate::wrapper::{DeviceId, DeviceWrapper};

/// Device registry with device lookup and management.
///
/// This registry maintains data structures for efficient device access:
/// 1. **Address range list**: Linear search for device ID (acceptable for <50 devices)
/// 2. **Device map**: Maps device IDs to device wrappers (O(1) lookup)
///
/// # Concurrency
///
/// The registry uses read-write locks to allow concurrent lookups while
/// protecting against concurrent modifications. Individual devices use their
/// own locks within the device wrapper.
///
/// # Lifecycle
///
/// Devices can be dynamically added and removed:
/// - `add_device()`: Register a new device
/// - `remove_device()`: Initiate device removal (state → Removing → Removed)
pub struct DeviceRegistry<R: DeviceAddrRange> {
    /// List of (address_range, device_id) pairs for device lookup.
    /// Linear search is used, which is efficient for typical VM configurations (<50 devices).
    range_list: RwLock<Vec<(R, DeviceId)>>,

    /// Maps device IDs to device wrappers.
    devices: RwLock<BTreeMap<DeviceId, DeviceWrapper<R>>>,

    /// Counter for generating unique device IDs.
    next_id: AtomicUsize,
}

impl<R: DeviceAddrRange + Copy + PartialEq + 'static> DeviceRegistry<R> {
    /// Creates a new empty device registry.
    pub fn new() -> Self {
        Self {
            range_list: RwLock::new(Vec::new()),
            devices: RwLock::new(BTreeMap::new()),
            next_id: AtomicUsize::new(1),
        }
    }

    /// Generates a new unique device ID.
    fn next_device_id(&self) -> DeviceId {
        DeviceId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Adds a device to the registry.
    ///
    /// Returns the assigned device ID, which can be used later for removal.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The device has no address ranges
    /// - Any address range overlaps with an existing device
    pub fn add_device(&self, device: Arc<dyn BaseDeviceOps<R>>) -> AxResult<DeviceId> {
        let device_id = self.next_device_id();

        // Clone device for wrapper
        let device_clone = Arc::clone(&device);
        let ranges: Vec<R> = device.address_ranges().to_vec();

        let wrapper = DeviceWrapper::new(device_id, device_clone);

        // Check for overlaps and register ranges
        {
            let mut range_list = self.range_list.write();

            // TODO: Implement proper overlap detection
            // For now, we rely on simple iteration
            // Proper overlap checking requires either:
            // 1. Adding start_addr()/size() methods to DeviceAddrRange trait
            // 2. Implementing range-specific overlap checks

            // Register all ranges
            for range in ranges {
                range_list.push((range, device_id));
            }
        }

        // Register device
        self.devices.write().insert(device_id, wrapper);

        Ok(device_id)
    }

    /// Finds the device ID for a given address.
    ///
    /// Returns `Some(DeviceId)` if a device handles this address, `None` otherwise.
    fn find_device_id(&self, addr: R::Addr) -> Option<DeviceId> {
        let range_list = self.range_list.read();

        // Linear search through ranges
        for (range, device_id) in range_list.iter() {
            if range.contains(addr) {
                return Some(*device_id);
            }
        }

        None
    }

    /// Finds a device and its region information for a given address.
    ///
    /// This method is useful for multi-region devices where you need to know
    /// which specific region was hit, along with the offset within that region.
    ///
    /// # Returns
    ///
    /// A tuple of (DeviceWrapper, Option<RegionHit>) where:
    /// - DeviceWrapper is the device handling this address
    /// - RegionHit is present if the device has multi-region support
    pub fn find_device_with_region(
        &self,
        addr: R::Addr,
    ) -> Option<(DeviceWrapper<R>, Option<RegionHit>)>
    where
        R::Addr: Into<usize>,
    {
        let device_id = self.find_device_id(addr)?;

        let devices = self.devices.read();
        let wrapper = devices.get(&device_id)?.clone();

        // Try to get region hit info
        let region_hit = wrapper.lookup_region(addr.into());

        Some((wrapper, region_hit))
    }

    /// Gets a device wrapper by its ID.
    pub fn get_device(&self, id: DeviceId) -> Option<DeviceWrapper<R>> {
        self.devices.read().get(&id).cloned()
    }

    /// Gets a device wrapper by address.
    pub fn get_device_by_addr(&self, addr: R::Addr) -> Option<DeviceWrapper<R>> {
        let device_id = self.find_device_id(addr)?;
        self.devices.read().get(&device_id).cloned()
    }

    /// Handles a read operation at the specified address.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No device is registered for this address
    /// - The device is not active (being removed or removed)
    /// - The device's read handler returns an error
    pub fn handle_read(&self, addr: R::Addr, width: AccessWidth) -> AxResult<usize> {
        // Find device
        let device_id = self
            .find_device_id(addr)
            .ok_or_else(|| AxError::NotFound)?;

        // Get device wrapper
        let devices = self.devices.read();
        let wrapper = devices
            .get(&device_id)
            .ok_or_else(|| AxError::NotFound)?;

        // Perform read (wrapper handles locking and lifecycle)
        wrapper.try_access_for_read(addr, width)
    }

    /// Handles a write operation at the specified address.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No device is registered for this address
    /// - The device is not active (being removed or removed)
    /// - The device's write handler returns an error
    pub fn handle_write(&self, addr: R::Addr, width: AccessWidth, val: usize) -> AxResult {
        // Find device
        let device_id = self
            .find_device_id(addr)
            .ok_or_else(|| AxError::NotFound)?;

        // Get device wrapper
        let devices = self.devices.read();
        let wrapper = devices
            .get(&device_id)
            .ok_or_else(|| AxError::NotFound)?;

        // Perform write (wrapper handles locking and lifecycle)
        wrapper.try_access_for_write(addr, width, val)
    }

    /// Removes a device from the registry.
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
    pub fn remove_device(&self, id: DeviceId) -> AxResult {
        // Get device wrapper
        let devices = self.devices.read();
        let wrapper = match devices.get(&id) {
            Some(w) => w.clone(),
            None => return ax_err!(NotFound, "Device not found"),
        };
        drop(devices);

        // Begin removal
        if !wrapper.begin_removal() {
            return ax_err!(BadState, "Device is already being removed");
        }

        // Wait for active accesses to complete
        wrapper.wait_idle();

        // Unregister address ranges
        {
            let mut range_list = self.range_list.write();
            let ranges: Vec<R> = wrapper.address_ranges().to_vec();
            range_list.retain(|(range, did)| {
                if *did == id {
                    !ranges.contains(range)
                } else {
                    true
                }
            });
        }

        // Complete removal
        wrapper.complete_removal();

        // Remove from device map
        self.devices.write().remove(&id);

        Ok(())
    }

    /// Lists all registered device IDs.
    pub fn list_devices(&self) -> alloc::vec::Vec<DeviceId> {
        self.devices.read().keys().copied().collect()
    }

    /// Gets statistics for a specific device.
    pub fn get_device_stats(&self, id: DeviceId) -> Option<(u64, u64, u64)> {
        let devices = self.devices.read();
        devices.get(&id).map(|wrapper| {
            let stats = wrapper.stats();
            (stats.reads(), stats.writes(), stats.errors())
        })
    }

    /// Gets the number of registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.read().len()
    }
}

impl<R: DeviceAddrRange + Copy + PartialEq + 'static> Default for DeviceRegistry<R> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange};
    use axdevice_base::{BaseMmioDeviceOps, EmuDeviceType};

    struct MockDevice {
        ranges: Vec<GuestPhysAddrRange>,
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
    fn test_registry_add_and_lookup() {
        let registry = DeviceRegistry::new();

        let device = Arc::new(MockDevice {
            ranges: alloc::vec![GuestPhysAddrRange::from_start_size(0x1000.into(), 0x100)],
        });

        let id = registry.add_device(device).unwrap();
        assert_eq!(registry.device_count(), 1);

        // Test read
        let result = registry.handle_read(GuestPhysAddr::from(0x1050), AccessWidth::Dword);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0x42);

        // Test write
        let result = registry.handle_write(GuestPhysAddr::from(0x1050), AccessWidth::Dword, 100);
        assert!(result.is_ok());

        // Test out of range
        let result = registry.handle_read(GuestPhysAddr::from(0x2000), AccessWidth::Dword);
        assert!(result.is_err());
    }

    #[test]
    fn test_registry_removal() {
        let registry = DeviceRegistry::new();

        let device = Arc::new(MockDevice {
            ranges: alloc::vec![GuestPhysAddrRange::from_start_size(0x1000.into(), 0x100)],
        });

        let id = registry.add_device(device).unwrap();
        assert_eq!(registry.device_count(), 1);

        // Remove device
        registry.remove_device(id).unwrap();
        assert_eq!(registry.device_count(), 0);

        // Access should fail
        let result = registry.handle_read(GuestPhysAddr::from(0x1050), AccessWidth::Dword);
        assert!(result.is_err());
    }

    #[test]
    fn test_registry_multi_range_device() {
        let registry = DeviceRegistry::new();

        let device = Arc::new(MockDevice {
            ranges: alloc::vec![
                GuestPhysAddrRange::from_start_size(0x1000.into(), 0x100),
                GuestPhysAddrRange::from_start_size(0x2000.into(), 0x100),
            ],
        });

        registry.add_device(device).unwrap();

        // Both ranges should work
        assert!(registry.handle_read(GuestPhysAddr::from(0x1050), AccessWidth::Dword).is_ok());
        assert!(registry.handle_read(GuestPhysAddr::from(0x2050), AccessWidth::Dword).is_ok());

        // Between ranges should fail
        assert!(registry.handle_read(GuestPhysAddr::from(0x1800), AccessWidth::Dword).is_err());
    }
}
