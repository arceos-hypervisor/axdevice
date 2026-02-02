//! Poll flag management for device notifications.
//!
//! This module provides atomic poll flags for devices that use polling-based
//! notification instead of interrupts. Poll flags are useful for:
//!
//! - High-frequency devices where interrupt overhead is unacceptable
//! - Low-latency scenarios where polling is more efficient
//! - Devices that need to batch multiple notifications

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, Ordering};

use spin::RwLock;

use crate::wrapper::DeviceId;

/// Poll flags manager for all devices.
///
/// Manages per-device atomic poll flags that can be set by devices and
/// checked by the vCPU loop.
pub struct PollFlags {
    /// Per-device poll flags (atomic for lock-free access).
    flags: RwLock<BTreeMap<DeviceId, AtomicU32>>,
}

impl PollFlags {
    /// Create a new poll flags manager.
    pub fn new() -> Self {
        Self {
            flags: RwLock::new(BTreeMap::new()),
        }
    }

    /// Register a device for polling.
    ///
    /// This should be called when a device with `NotifyMethod::Poll` is registered.
    pub fn register(&self, device_id: DeviceId) {
        self.flags.write().insert(device_id, AtomicU32::new(0));
    }

    /// Unregister a device from polling.
    ///
    /// This should be called when a device is unregistered.
    pub fn unregister(&self, device_id: DeviceId) {
        self.flags.write().remove(&device_id);
    }

    /// Set poll flags for a device (atomic OR).
    ///
    /// This is called by the device when it wants to notify the guest.
    /// Multiple flags can be OR'd together.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    /// * `flags` - The flags to set (bitwise OR with existing flags).
    ///
    /// # Returns
    ///
    /// The previous flag value.
    #[inline]
    pub fn set(&self, device_id: DeviceId, flags: u32) -> u32 {
        if let Some(flag) = self.flags.read().get(&device_id) {
            flag.fetch_or(flags, Ordering::Release)
        } else {
            0
        }
    }

    /// Check and clear poll flags for a device (atomic swap).
    ///
    /// This is called by the vCPU loop to check for pending notifications.
    /// The flags are cleared after reading.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    ///
    /// # Returns
    ///
    /// The flag value before clearing.
    #[inline]
    pub fn check_and_clear(&self, device_id: DeviceId) -> u32 {
        if let Some(flag) = self.flags.read().get(&device_id) {
            flag.swap(0, Ordering::AcqRel)
        } else {
            0
        }
    }

    /// Peek at poll flags for a device (read-only, no clear).
    ///
    /// This is useful for checking if there are any pending notifications
    /// without clearing them.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    ///
    /// # Returns
    ///
    /// The current flag value.
    #[inline]
    pub fn peek(&self, device_id: DeviceId) -> u32 {
        if let Some(flag) = self.flags.read().get(&device_id) {
            flag.load(Ordering::Acquire)
        } else {
            0
        }
    }

    /// Clear poll flags for a device.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    #[inline]
    pub fn clear(&self, device_id: DeviceId) {
        if let Some(flag) = self.flags.read().get(&device_id) {
            flag.store(0, Ordering::Release);
        }
    }

    /// Check if a device is registered for polling.
    pub fn contains(&self, device_id: DeviceId) -> bool {
        self.flags.read().contains_key(&device_id)
    }

    /// Get the number of registered devices.
    pub fn len(&self) -> usize {
        self.flags.read().len()
    }

    /// Check if no devices are registered.
    pub fn is_empty(&self) -> bool {
        self.flags.read().is_empty()
    }

    /// Check if any device has pending poll flags.
    ///
    /// This is useful for the vCPU loop to quickly check if there's any
    /// polling work to do.
    pub fn has_any_pending(&self) -> bool {
        self.flags
            .read()
            .values()
            .any(|flag| flag.load(Ordering::Relaxed) != 0)
    }

    /// Get all devices with pending poll flags.
    ///
    /// Returns a list of (device_id, flags) pairs for devices that have
    /// non-zero poll flags.
    pub fn get_all_pending(&self) -> alloc::vec::Vec<(DeviceId, u32)> {
        self.flags
            .read()
            .iter()
            .filter_map(|(id, flag)| {
                let value = flag.load(Ordering::Acquire);
                if value != 0 {
                    Some((*id, value))
                } else {
                    None
                }
            })
            .collect()
    }
}

impl Default for PollFlags {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poll_flags_basic() {
        let poll = PollFlags::new();
        let device_id = DeviceId(1);

        // Register device
        poll.register(device_id);
        assert!(poll.contains(device_id));
        assert_eq!(poll.peek(device_id), 0);

        // Set flags
        poll.set(device_id, 0b0001);
        assert_eq!(poll.peek(device_id), 0b0001);

        // Set more flags (OR)
        poll.set(device_id, 0b0010);
        assert_eq!(poll.peek(device_id), 0b0011);

        // Check and clear
        let flags = poll.check_and_clear(device_id);
        assert_eq!(flags, 0b0011);
        assert_eq!(poll.peek(device_id), 0);

        // Unregister
        poll.unregister(device_id);
        assert!(!poll.contains(device_id));
    }

    #[test]
    fn test_poll_flags_multiple_devices() {
        let poll = PollFlags::new();
        let device1 = DeviceId(1);
        let device2 = DeviceId(2);

        poll.register(device1);
        poll.register(device2);

        poll.set(device1, 0b0001);
        poll.set(device2, 0b0010);

        assert_eq!(poll.peek(device1), 0b0001);
        assert_eq!(poll.peek(device2), 0b0010);

        // Get all pending
        let pending = poll.get_all_pending();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn test_poll_flags_unregistered() {
        let poll = PollFlags::new();
        let device_id = DeviceId(999);

        // Operations on unregistered device should be safe
        assert_eq!(poll.peek(device_id), 0);
        assert_eq!(poll.check_and_clear(device_id), 0);
        poll.set(device_id, 0xFFFF);
        assert_eq!(poll.peek(device_id), 0); // Not registered, so no effect
    }
}
