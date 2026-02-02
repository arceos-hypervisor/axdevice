//! Device notifier implementation.
//!
//! This module provides the concrete implementation of `DeviceNotifier` that devices
//! use to send notifications. It forwards notification requests to the notification manager.

use alloc::sync::Arc;

use axdevice_base::{DeviceEvent, DeviceNotifier, IrqType, NotifyMethod};
use axerrno::AxResult;

use crate::wrapper::DeviceId;

use super::manager::DeviceNotificationManager;

/// Device notifier implementation.
///
/// This is the concrete implementation of `DeviceNotifier` that devices use
/// to send notifications. It forwards notification requests to the notification manager.
///
/// # Design
///
/// The notifier uses dependency injection pattern:
/// 1. Created by the device framework during device registration
/// 2. Injected into the device via `set_notifier()`
/// 3. Used by the device to send notifications without knowing the implementation
///
/// This decouples devices from the notification management system and makes
/// devices testable with mock notifiers.
///
/// # Notification Methods
///
/// The notifier supports multiple notification methods:
/// - **Interrupt**: Traditional interrupt injection (default)
/// - **Poll**: Sets atomic poll flags for high-frequency devices
/// - **Event**: Adds to event queue for batch processing
/// - **Callback**: Executes callback synchronously (for testing)
pub struct DeviceNotifierImpl {
    /// The device ID this notifier belongs to.
    device_id: DeviceId,

    /// Reference to the notification manager.
    manager: Arc<DeviceNotificationManager>,

    /// Notification method for this device.
    method: NotifyMethod,

    /// Synchronous callback (for Callback method).
    callback: Option<Arc<dyn Fn(DeviceEvent) + Send + Sync>>,
}

impl DeviceNotifierImpl {
    /// Creates a new notifier.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    /// * `manager` - Reference to the notification manager.
    pub fn new(device_id: DeviceId, manager: Arc<DeviceNotificationManager>) -> Self {
        Self {
            device_id,
            manager,
            method: NotifyMethod::Interrupt,
            callback: None,
        }
    }

    /// Creates a new notifier with a specific notification method.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    /// * `manager` - Reference to the notification manager.
    /// * `method` - The notification method to use.
    pub fn with_method(device_id: DeviceId, manager: Arc<DeviceNotificationManager>, method: NotifyMethod) -> Self {
        Self {
            device_id,
            manager,
            method,
            callback: None,
        }
    }

    /// Sets a callback function for Callback notification method.
    ///
    /// This is primarily useful for testing, where you want to synchronously
    /// verify that a device sent a notification.
    ///
    /// # Arguments
    ///
    /// * `callback` - The callback function to execute on notification.
    pub fn with_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(DeviceEvent) + Send + Sync + 'static,
    {
        self.callback = Some(Arc::new(callback));
        self
    }

    /// Gets the device ID for this notifier.
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }
}

impl DeviceNotifier for DeviceNotifierImpl {
    fn notify(&self, event: DeviceEvent) -> AxResult {
        match self.method {
            NotifyMethod::Callback => {
                // Synchronous callback
                if let Some(ref callback) = self.callback {
                    callback(event);
                }
                Ok(())
            }
            _ => {
                // All other methods go through the manager
                self.manager.inject(self.device_id, event)
            }
        }
    }

    fn clear(&self, event: DeviceEvent) -> AxResult {
        // TODO: Implement notification clearing for level-triggered interrupts
        trace!(
            "Device {:?} clearing notification {:?}",
            self.device_id,
            event
        );
        Ok(())
    }

    fn method(&self) -> NotifyMethod {
        self.method
    }

    fn has_pending(&self) -> bool {
        if self.method == NotifyMethod::Poll {
            self.manager.peek_poll(self.device_id) != 0
        } else {
            false
        }
    }
}

/// Legacy type alias for backward compatibility.
///
/// Devices that were using `DeviceInterruptTrigger` can continue to use it.
/// The type is aliased to `DeviceNotifierImpl` which implements both
/// `DeviceNotifier` and `InterruptTrigger` (via the blanket impl in axdevice_base).
pub type DeviceInterruptTrigger = DeviceNotifierImpl;

#[cfg(test)]
mod tests {
    use super::*;
    use axdevice_base::{CpuAffinity, NotificationConfig, TriggerMode};
    use core::sync::atomic::{AtomicU32, Ordering};

    fn create_test_config(irq: u32) -> NotificationConfig {
        NotificationConfig::interrupt(irq)
            .with_cpu_affinity(CpuAffinity::Fixed(0))
            .with_priority(100)
    }

    #[test]
    fn test_device_notifier_interrupt() {
        let manager = Arc::new(DeviceNotificationManager::new(1));
        let device_id = DeviceId(1);
        let config = create_test_config(32);

        // Register device
        manager.register_notification(device_id, config).unwrap();

        // Create notifier
        let notifier = DeviceNotifierImpl::new(device_id, Arc::clone(&manager));
        assert_eq!(notifier.method(), NotifyMethod::Interrupt);

        // Notify
        notifier.notify(DeviceEvent::DataReady).unwrap();

        // Verify notification was injected
        assert_eq!(manager.pending_count(0), 1);
        let pending = manager.pop_pending(0).unwrap();
        assert_eq!(pending.irq, 32);
        assert_eq!(pending.device_id, device_id);
        assert_eq!(pending.event, DeviceEvent::DataReady);
    }

    #[test]
    fn test_device_notifier_poll() {
        let manager = Arc::new(DeviceNotificationManager::new(1));
        let device_id = DeviceId(1);
        let config = NotificationConfig::poll();

        // Register device
        manager.register_notification(device_id, config).unwrap();

        // Create notifier with poll method
        let notifier = DeviceNotifierImpl::with_method(
            device_id,
            Arc::clone(&manager),
            NotifyMethod::Poll
        );
        assert_eq!(notifier.method(), NotifyMethod::Poll);

        // Initially no pending
        assert!(!notifier.has_pending());

        // Notify
        notifier.notify(DeviceEvent::DataReady).unwrap();

        // Verify poll flag was set
        assert!(notifier.has_pending());
        let flags = manager.check_poll(device_id);
        assert!(DeviceEvent::DataReady.is_set_in(flags));

        // After check, no longer pending
        assert!(!notifier.has_pending());
    }

    #[test]
    fn test_device_notifier_callback() {
        let manager = Arc::new(DeviceNotificationManager::new(1));
        let device_id = DeviceId(1);

        // Counter to track callback invocations
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        // Create notifier with callback
        let notifier = DeviceNotifierImpl::with_method(
            device_id,
            Arc::clone(&manager),
            NotifyMethod::Callback
        ).with_callback(move |event| {
            if event == DeviceEvent::DataReady {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }
        });

        // Notify multiple times
        notifier.notify(DeviceEvent::DataReady).unwrap();
        notifier.notify(DeviceEvent::DataReady).unwrap();
        notifier.notify(DeviceEvent::SpaceAvailable).unwrap(); // Different event

        // Verify callback was invoked
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_device_notifier_legacy_trigger() {
        use axdevice_base::InterruptTrigger;

        let manager = Arc::new(DeviceNotificationManager::new(1));
        let device_id = DeviceId(1);
        let config = create_test_config(32);

        // Register device
        manager.register_notification(device_id, config).unwrap();

        // Create notifier (DeviceInterruptTrigger is an alias for DeviceNotifierImpl)
        let trigger: Arc<dyn InterruptTrigger> = Arc::new(
            DeviceInterruptTrigger::new(device_id, Arc::clone(&manager))
        );

        // Use legacy trigger API
        trigger.trigger(IrqType::Primary).unwrap();

        // Verify interrupt was injected
        assert_eq!(manager.pending_count(0), 1);
        let pending = manager.pop_pending(0).unwrap();
        assert_eq!(pending.irq, 32);
    }

    #[test]
    fn test_device_notifier_additional_irq() {
        let manager = Arc::new(DeviceNotificationManager::new(1));
        let device_id = DeviceId(1);

        let mut config = create_test_config(32);
        config.additional_irqs = alloc::vec![33, 34];

        manager.register_notification(device_id, config).unwrap();

        let notifier = DeviceNotifierImpl::new(device_id, Arc::clone(&manager));

        // Trigger primary
        notifier.notify(DeviceEvent::Irq(IrqType::Primary)).unwrap();
        let p1 = manager.pop_pending(0).unwrap();
        assert_eq!(p1.irq, 32);

        // Trigger additional
        notifier.notify(DeviceEvent::Irq(IrqType::Additional(0))).unwrap();
        let p2 = manager.pop_pending(0).unwrap();
        assert_eq!(p2.irq, 33);

        notifier.notify(DeviceEvent::Irq(IrqType::Additional(1))).unwrap();
        let p3 = manager.pop_pending(0).unwrap();
        assert_eq!(p3.irq, 34);
    }
}
