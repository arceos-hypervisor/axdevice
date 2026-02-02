//! Notification routing table for device notification management.
//!
//! This module provides the routing table that maps device IDs to their
//! notification configurations, supporting both the new `NotificationConfig`
//! and the legacy `InterruptConfig` for backward compatibility.

use alloc::collections::BTreeMap;
use axdevice_base::{InterruptConfig, NotificationConfig};
use axerrno::{ax_err, AxResult};
use spin::RwLock;

use crate::wrapper::DeviceId;

/// Notification routing table.
///
/// Maps device IDs to their notification configurations. Used to look up
/// notification settings when a device triggers a notification.
///
/// This table supports both:
/// - `NotificationConfig` (new API) for devices using the new notification system
/// - `InterruptConfig` (legacy API) for backward compatibility
pub struct RoutingTable {
    /// Notification configurations indexed by device ID.
    table: RwLock<BTreeMap<DeviceId, NotificationConfig>>,
}

impl RoutingTable {
    /// Creates a new empty routing table.
    pub fn new() -> Self {
        Self {
            table: RwLock::new(BTreeMap::new()),
        }
    }

    /// Registers a device's notification configuration.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    /// * `config` - The notification configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the device ID is already registered.
    pub fn register_notification(&self, device_id: DeviceId, config: NotificationConfig) -> AxResult {
        let mut table = self.table.write();

        if table.contains_key(&device_id) {
            return ax_err!(AlreadyExists, "Device notification already registered");
        }

        table.insert(device_id, config);
        Ok(())
    }

    /// Registers a device's interrupt configuration (legacy API).
    ///
    /// This method converts `InterruptConfig` to `NotificationConfig` internally
    /// for backward compatibility.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    /// * `config` - The interrupt configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the device ID is already registered.
    #[allow(deprecated)]
    pub fn register(&self, device_id: DeviceId, config: InterruptConfig) -> AxResult {
        self.register_notification(device_id, NotificationConfig::from_interrupt_config(&config))
    }

    /// Unregisters a device's notification configuration.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the device ID is not found.
    pub fn unregister(&self, device_id: DeviceId) -> AxResult {
        let mut table = self.table.write();

        if table.remove(&device_id).is_none() {
            return ax_err!(NotFound, "Device notification not found");
        }

        Ok(())
    }

    /// Gets the notification configuration for a device.
    ///
    /// Returns `None` if the device is not registered.
    pub fn get(&self, device_id: DeviceId) -> Option<NotificationConfig> {
        self.table.read().get(&device_id).cloned()
    }

    /// Gets the interrupt configuration for a device (legacy API).
    ///
    /// This method converts `NotificationConfig` to `InterruptConfig` for
    /// backward compatibility. Returns `None` if the device is not registered
    /// or if the device doesn't use interrupt-based notification.
    #[allow(deprecated)]
    pub fn get_interrupt_config(&self, device_id: DeviceId) -> Option<InterruptConfig> {
        self.table.read().get(&device_id)?.to_interrupt_config()
    }

    /// Checks if a device is registered.
    pub fn contains(&self, device_id: DeviceId) -> bool {
        self.table.read().contains_key(&device_id)
    }

    /// Gets the number of registered devices.
    pub fn len(&self) -> usize {
        self.table.read().len()
    }

    /// Checks if the routing table is empty.
    pub fn is_empty(&self) -> bool {
        self.table.read().is_empty()
    }
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axdevice_base::{CpuAffinity, NotifyMethod, TriggerMode};

    fn create_test_interrupt_config(irq: u32) -> InterruptConfig {
        InterruptConfig {
            primary_irq: irq,
            additional_irqs: alloc::vec![],
            trigger_mode: TriggerMode::Level,
            cpu_affinity: CpuAffinity::Fixed(0),
            priority: 100,
        }
    }

    fn create_test_notification_config(irq: u32) -> NotificationConfig {
        NotificationConfig::interrupt(irq)
            .with_priority(100)
    }

    #[test]
    fn test_routing_table_notification_config() {
        let table = RoutingTable::new();
        assert!(table.is_empty());

        let device_id = DeviceId(1);
        let config = create_test_notification_config(32);

        // Register device
        table.register_notification(device_id, config).unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.contains(device_id));

        // Get config
        let retrieved = table.get(device_id).unwrap();
        assert_eq!(retrieved.primary_irq, Some(32));
        assert_eq!(retrieved.method, NotifyMethod::Interrupt);

        // Unregister device
        table.unregister(device_id).unwrap();
        assert!(table.is_empty());
    }

    #[test]
    fn test_routing_table_legacy_interrupt_config() {
        let table = RoutingTable::new();
        let device_id = DeviceId(1);
        let config = create_test_interrupt_config(32);

        // Register using legacy API
        table.register(device_id, config).unwrap();

        // Get using new API
        let retrieved = table.get(device_id).unwrap();
        assert_eq!(retrieved.primary_irq, Some(32));
        assert_eq!(retrieved.method, NotifyMethod::Interrupt);

        // Get using legacy API
        let legacy = table.get_interrupt_config(device_id).unwrap();
        assert_eq!(legacy.primary_irq, 32);
    }

    #[test]
    fn test_routing_table_poll_config() {
        let table = RoutingTable::new();
        let device_id = DeviceId(1);
        let config = NotificationConfig::poll();

        // Register poll-based device
        table.register_notification(device_id, config).unwrap();

        // Get config
        let retrieved = table.get(device_id).unwrap();
        assert_eq!(retrieved.method, NotifyMethod::Poll);
        assert_eq!(retrieved.primary_irq, None);

        // Legacy API should return None for poll-based devices
        assert!(table.get_interrupt_config(device_id).is_none());
    }

    #[test]
    fn test_routing_table_duplicate_register() {
        let table = RoutingTable::new();
        let device_id = DeviceId(1);
        let config = create_test_notification_config(32);

        table.register_notification(device_id, config.clone()).unwrap();
        let result = table.register_notification(device_id, config);
        assert!(result.is_err());
    }

    #[test]
    fn test_routing_table_unregister_not_found() {
        let table = RoutingTable::new();
        let device_id = DeviceId(999);

        let result = table.unregister(device_id);
        assert!(result.is_err());
    }
}
