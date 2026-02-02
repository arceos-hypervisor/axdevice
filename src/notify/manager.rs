//! Device notification manager.
//!
//! This module provides the `DeviceNotificationManager` which handles notification routing,
//! scheduling, and queuing for all emulated devices in a VM.
//!
//! # Notification Methods
//!
//! The manager supports multiple notification methods:
//! - **Interrupt**: Traditional interrupt injection via priority queue
//! - **Poll**: Atomic poll flags for high-frequency devices
//! - **Event**: Event queue for batch processing
//! - **Callback**: Handled at the notifier level (not queued)

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use axdevice_base::{CpuAffinity, DeviceEvent, InterruptConfig, IrqType, NotificationConfig, NotifyMethod};
use axerrno::{ax_err, AxResult};
use spin::Mutex;

use crate::wrapper::DeviceId;

use super::{
    poll::PollFlags,
    queue::{EntryId, PendingNotification, TransactionalNotificationQueue},
    routing::RoutingTable,
};

/// Device notification manager.
///
/// Manages notification routing, scheduling, and queuing for all devices in a VM.
/// Supports multiple notification methods including interrupts, polling, and events.
///
/// # Architecture
///
/// - **Routing Table**: Maps device IDs to notification configurations
/// - **Priority Queues**: Per-CPU queues for pending interrupt-based notifications
/// - **Poll Flags**: Atomic flags for poll-based devices
/// - **Event Queues**: Per-CPU queues for event-based notifications
/// - **CPU Selection**: Implements various affinity strategies
///
/// # Usage
///
/// ```rust,ignore
/// let manager = DeviceNotificationManager::new(4); // 4 vCPUs
///
/// // Register a device's notification
/// manager.register_notification(device_id, NotificationConfig::interrupt(32))?;
///
/// // Inject a notification (called by device notifier)
/// manager.inject(device_id, DeviceEvent::DataReady)?;
///
/// // Pop pending interrupt for a vCPU (called before VM entry)
/// if let Some(pending) = manager.pop_pending(cpu_id) {
///     vcpu.inject_interrupt(pending.irq);
/// }
///
/// // Or check poll flags for a device
/// let flags = manager.check_poll(device_id);
/// if DeviceEvent::DataReady.is_set_in(flags) {
///     // Handle data ready...
/// }
/// ```
pub struct DeviceNotificationManager {
    /// Routing table mapping device IDs to notification configurations.
    routing_table: RoutingTable,

    /// Per-CPU transactional priority queues for pending interrupts.
    interrupt_queues: Vec<Mutex<TransactionalNotificationQueue>>,

    /// Poll flags for poll-based devices.
    poll_flags: PollFlags,

    /// Per-CPU event queues for event-based notifications.
    event_queues: Vec<Mutex<VecDeque<PendingNotification>>>,

    /// Counter for round-robin CPU selection.
    next_cpu: AtomicU64,

    /// Global timestamp counter for notification ordering.
    timestamp: AtomicU64,

    /// Number of CPUs.
    cpu_count: usize,
}

impl DeviceNotificationManager {
    /// Creates a new notification manager.
    ///
    /// # Arguments
    ///
    /// * `cpu_count` - Number of vCPUs in the VM.
    pub fn new(cpu_count: usize) -> Self {
        let mut interrupt_queues = Vec::with_capacity(cpu_count);
        let mut event_queues = Vec::with_capacity(cpu_count);

        for _ in 0..cpu_count {
            interrupt_queues.push(Mutex::new(TransactionalNotificationQueue::new()));
            event_queues.push(Mutex::new(VecDeque::new()));
        }

        Self {
            routing_table: RoutingTable::new(),
            interrupt_queues,
            poll_flags: PollFlags::new(),
            event_queues,
            next_cpu: AtomicU64::new(0),
            timestamp: AtomicU64::new(0),
            cpu_count,
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
    /// Returns an error if the device is already registered.
    pub fn register_notification(&self, device_id: DeviceId, config: NotificationConfig) -> AxResult {
        debug!(
            "Registering notification for device {:?}: method={:?}, irq={:?}, priority={}",
            device_id, config.method, config.primary_irq, config.priority
        );

        // If using Poll method, register the poll flag
        if config.method == NotifyMethod::Poll {
            self.poll_flags.register(device_id);
        }

        self.routing_table.register_notification(device_id, config)
    }

    /// Registers a device's interrupt configuration (legacy API).
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    /// * `config` - The interrupt configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is already registered.
    #[allow(deprecated)]
    pub fn register(&self, device_id: DeviceId, config: InterruptConfig) -> AxResult {
        debug!(
            "Registering interrupt for device {:?}: IRQ {}, priority {}",
            device_id, config.primary_irq, config.priority
        );
        self.routing_table.register(device_id, config)
    }

    /// Unregisters a device's notification configuration.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not found.
    pub fn unregister(&self, device_id: DeviceId) -> AxResult {
        debug!("Unregistering notification for device {:?}", device_id);
        self.poll_flags.unregister(device_id);
        self.routing_table.unregister(device_id)
    }

    /// Injects a notification for a device.
    ///
    /// This method routes the notification based on the device's configured method:
    /// - Interrupt: Adds to the interrupt priority queue
    /// - Poll: Sets the poll flag
    /// - Event: Adds to the event queue
    /// - Callback: Not handled here (handled at the notifier level)
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device requesting the notification.
    /// * `event` - The event type.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not registered.
    pub fn inject(&self, device_id: DeviceId, event: DeviceEvent) -> AxResult {
        let config = self.routing_table.get(device_id)
            .ok_or_else(|| axerrno::ax_err_type!(NotFound, "Device not registered"))?;

        match config.method {
            NotifyMethod::Interrupt => {
                self.inject_interrupt(device_id, &config, event)
            }
            NotifyMethod::Poll => {
                self.inject_poll(device_id, event)
            }
            NotifyMethod::Event => {
                self.inject_event(device_id, &config, event)
            }
            NotifyMethod::Callback => {
                // Callback is handled at the notifier level
                Ok(())
            }
        }
    }

    /// Injects an interrupt-based notification.
    fn inject_interrupt(&self, device_id: DeviceId, config: &NotificationConfig, event: DeviceEvent) -> AxResult {
        // Get the IRQ number based on the event
        let irq = match event {
            DeviceEvent::Irq(IrqType::Primary) | DeviceEvent::DataReady | DeviceEvent::SpaceAvailable | DeviceEvent::ConfigChanged => {
                config.primary_irq.ok_or_else(|| axerrno::ax_err_type!(InvalidInput, "No primary IRQ configured"))?
            }
            DeviceEvent::Irq(IrqType::Additional(idx)) => {
                if idx as usize >= config.additional_irqs.len() {
                    return ax_err!(InvalidInput, "Invalid additional IRQ index");
                }
                config.additional_irqs[idx as usize]
            }
            DeviceEvent::Custom(_) => {
                config.primary_irq.ok_or_else(|| axerrno::ax_err_type!(InvalidInput, "No IRQ configured for custom event"))?
            }
        };

        // Select target CPU
        let cpu_id = self.select_target_cpu(config);

        // Create pending notification
        let timestamp = self.timestamp.fetch_add(1, Ordering::Relaxed);
        let pending = PendingNotification::with_event(irq, config.priority, device_id, timestamp, event);

        // Add to the confirmed queue
        if cpu_id < self.interrupt_queues.len() {
            self.interrupt_queues[cpu_id].lock().push(pending);
            Ok(())
        } else {
            ax_err!(InvalidInput, "Invalid CPU ID")
        }
    }

    /// Injects a poll-based notification.
    fn inject_poll(&self, device_id: DeviceId, event: DeviceEvent) -> AxResult {
        self.poll_flags.set(device_id, event.as_flag());
        Ok(())
    }

    /// Injects an event-based notification.
    fn inject_event(&self, device_id: DeviceId, config: &NotificationConfig, event: DeviceEvent) -> AxResult {
        let cpu_id = self.select_target_cpu(config);
        let timestamp = self.timestamp.fetch_add(1, Ordering::Relaxed);

        // For event-based, IRQ is optional
        let irq = config.primary_irq.unwrap_or(0);
        let pending = PendingNotification::with_event(irq, config.priority, device_id, timestamp, event);

        if cpu_id < self.event_queues.len() {
            self.event_queues[cpu_id].lock().push_back(pending);
            Ok(())
        } else {
            ax_err!(InvalidInput, "Invalid CPU ID")
        }
    }

    /// Injects an interrupt for a device using the legacy IrqType API.
    ///
    /// This method is provided for backward compatibility with devices using
    /// the old `InterruptTrigger` API.
    pub fn inject_irq(&self, device_id: DeviceId, irq_type: IrqType) -> AxResult {
        self.inject(device_id, DeviceEvent::Irq(irq_type))
    }

    /// Injects an interrupt in pending state (transactional, phase 1).
    ///
    /// This method adds the interrupt to the queue in an uncommitted state.
    /// Use `confirm_pending()` to finalize or `rollback_pending()` to cancel.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device requesting the interrupt.
    /// * `irq_type` - The type of interrupt (Primary or Additional).
    ///
    /// # Returns
    ///
    /// A tuple of (cpu_id, entry_id) for use with confirm/rollback.
    pub fn inject_pending(&self, device_id: DeviceId, irq_type: IrqType) -> AxResult<(usize, EntryId)> {
        let config = self.routing_table.get(device_id)
            .ok_or_else(|| axerrno::ax_err_type!(NotFound, "Device not registered"))?;

        // Get the IRQ number based on the type
        let irq = match irq_type {
            IrqType::Primary => {
                config.primary_irq.ok_or_else(|| axerrno::ax_err_type!(InvalidInput, "No primary IRQ configured"))?
            }
            IrqType::Additional(idx) => {
                if idx as usize >= config.additional_irqs.len() {
                    return ax_err!(InvalidInput, "Invalid additional IRQ index");
                }
                config.additional_irqs[idx as usize]
            }
        };

        // Select target CPU
        let cpu_id = self.select_target_cpu(&config);

        if cpu_id >= self.interrupt_queues.len() {
            return ax_err!(InvalidInput, "Invalid CPU ID");
        }

        // Create pending notification
        let timestamp = self.timestamp.fetch_add(1, Ordering::Relaxed);
        let pending = PendingNotification::with_event(
            irq, config.priority, device_id, timestamp,
            DeviceEvent::Irq(irq_type)
        );

        // Add to the queue in pending state
        let entry_id = self.interrupt_queues[cpu_id].lock().push_pending(pending);

        trace!(
            "Injected pending IRQ {} from device {:?} to CPU {} (entry_id: {})",
            irq,
            device_id,
            cpu_id,
            entry_id
        );

        Ok((cpu_id, entry_id))
    }

    /// Confirms a pending interrupt entry (transactional, phase 2 - success).
    pub fn confirm_pending(&self, cpu_id: usize, entry_id: EntryId) {
        if cpu_id < self.interrupt_queues.len() {
            self.interrupt_queues[cpu_id].lock().confirm(entry_id);
            trace!("Confirmed pending entry {} on CPU {}", entry_id, cpu_id);
        }
    }

    /// Rolls back a pending interrupt entry (transactional, phase 2 - failure).
    pub fn rollback_pending(&self, cpu_id: usize, entry_id: EntryId) {
        if cpu_id < self.interrupt_queues.len() {
            self.interrupt_queues[cpu_id].lock().rollback(entry_id);
            trace!("Rolled back pending entry {} on CPU {}", entry_id, cpu_id);
        }
    }

    /// Pops the highest-priority pending interrupt for a CPU.
    ///
    /// This should be called before VM entry to inject pending interrupts into the vCPU.
    pub fn pop_pending(&self, cpu_id: usize) -> Option<PendingNotification> {
        if cpu_id < self.interrupt_queues.len() {
            self.interrupt_queues[cpu_id].lock().pop()
        } else {
            None
        }
    }

    /// Gets the number of pending interrupts for a CPU.
    pub fn pending_count(&self, cpu_id: usize) -> usize {
        if cpu_id < self.interrupt_queues.len() {
            self.interrupt_queues[cpu_id].lock().len()
        } else {
            0
        }
    }

    /// Checks and clears poll flags for a device.
    ///
    /// Returns the poll flags that were set since the last check.
    pub fn check_poll(&self, device_id: DeviceId) -> u32 {
        self.poll_flags.check_and_clear(device_id)
    }

    /// Peeks at poll flags for a device without clearing.
    pub fn peek_poll(&self, device_id: DeviceId) -> u32 {
        self.poll_flags.peek(device_id)
    }

    /// Drains events from the event queue for a CPU.
    ///
    /// # Arguments
    ///
    /// * `cpu_id` - The CPU ID.
    /// * `max` - Maximum number of events to drain.
    ///
    /// # Returns
    ///
    /// A vector of pending notifications.
    pub fn drain_events(&self, cpu_id: usize, max: usize) -> Vec<PendingNotification> {
        if let Some(queue) = self.event_queues.get(cpu_id) {
            let mut queue = queue.lock();
            let count = queue.len().min(max);
            queue.drain(..count).collect()
        } else {
            Vec::new()
        }
    }

    /// Selects the target CPU for a notification based on affinity strategy.
    fn select_target_cpu(&self, config: &NotificationConfig) -> usize {
        match &config.cpu_affinity {
            CpuAffinity::Fixed(cpu_id) => {
                *cpu_id % self.cpu_count
            }
            CpuAffinity::RoundRobin => {
                let cpu = self.next_cpu.fetch_add(1, Ordering::Relaxed);
                cpu as usize % self.cpu_count
            }
            CpuAffinity::LoadBalance => {
                // Select CPU with shortest queue
                let mut min_len = usize::MAX;
                let mut selected_cpu = 0;

                for (cpu_id, queue) in self.interrupt_queues.iter().enumerate() {
                    let len = queue.lock().len();
                    if len < min_len {
                        min_len = len;
                        selected_cpu = cpu_id;
                    }
                }

                selected_cpu
            }
            CpuAffinity::Broadcast => {
                // TODO: Implement true broadcast
                warn!("Broadcast mode not fully implemented, using CPU 0");
                0
            }
        }
    }

    /// Injects an interrupt directly by IRQ number (for passthrough devices).
    pub fn inject_raw(&self, irq: u32, cpu_id: usize, priority: u8) -> AxResult {
        if cpu_id >= self.interrupt_queues.len() {
            return ax_err!(InvalidInput, "Invalid CPU ID");
        }

        let timestamp = self.timestamp.fetch_add(1, Ordering::Relaxed);
        let device_id = DeviceId::passthrough(irq);
        let pending = PendingNotification::new(irq, priority, device_id, timestamp);

        self.interrupt_queues[cpu_id].lock().push(pending);

        Ok(())
    }

    /// Clears all pending notifications (useful for VM reset).
    pub fn clear_all_pending(&self) {
        for queue in &self.interrupt_queues {
            queue.lock().clear_all();
        }
        for queue in &self.event_queues {
            queue.lock().clear();
        }
    }

    /// Gets the number of uncommitted pending entries for a CPU.
    pub fn uncommitted_count(&self, cpu_id: usize) -> usize {
        if cpu_id < self.interrupt_queues.len() {
            self.interrupt_queues[cpu_id].lock().pending_count()
        } else {
            0
        }
    }

    /// Gets the number of registered devices.
    pub fn registered_device_count(&self) -> usize {
        self.routing_table.len()
    }

    /// Checks if any device has pending poll flags.
    pub fn has_any_poll_pending(&self) -> bool {
        self.poll_flags.has_any_pending()
    }

    /// Gets all devices with pending poll flags.
    pub fn get_all_poll_pending(&self) -> Vec<(DeviceId, u32)> {
        self.poll_flags.get_all_pending()
    }
}

/// Legacy type alias for backward compatibility.
pub type DeviceInterruptManager = DeviceNotificationManager;

#[cfg(test)]
mod tests {
    use super::*;
    use axdevice_base::TriggerMode;

    fn create_test_interrupt_config(irq: u32, cpu: usize) -> InterruptConfig {
        InterruptConfig {
            primary_irq: irq,
            additional_irqs: alloc::vec![],
            trigger_mode: TriggerMode::Level,
            cpu_affinity: CpuAffinity::Fixed(cpu),
            priority: 100,
        }
    }

    fn create_test_notification_config(irq: u32, cpu: usize) -> NotificationConfig {
        NotificationConfig::interrupt(irq)
            .with_cpu_affinity(CpuAffinity::Fixed(cpu))
            .with_priority(100)
    }

    #[test]
    fn test_notification_manager_basic() {
        let manager = DeviceNotificationManager::new(2);
        let device_id = DeviceId(1);
        let config = create_test_notification_config(32, 0);

        // Register device
        manager.register_notification(device_id, config).unwrap();
        assert_eq!(manager.registered_device_count(), 1);

        // Inject notification
        manager.inject(device_id, DeviceEvent::DataReady).unwrap();
        assert_eq!(manager.pending_count(0), 1);

        // Pop notification
        let pending = manager.pop_pending(0).unwrap();
        assert_eq!(pending.irq, 32);
        assert_eq!(pending.device_id, device_id);
        assert_eq!(pending.event, DeviceEvent::DataReady);
        assert_eq!(manager.pending_count(0), 0);
    }

    #[test]
    fn test_notification_manager_poll() {
        let manager = DeviceNotificationManager::new(1);
        let device_id = DeviceId(1);
        let config = NotificationConfig::poll();

        // Register device
        manager.register_notification(device_id, config).unwrap();

        // Inject poll notification
        manager.inject(device_id, DeviceEvent::DataReady).unwrap();
        manager.inject(device_id, DeviceEvent::SpaceAvailable).unwrap();

        // Check poll flags
        let flags = manager.check_poll(device_id);
        assert!(DeviceEvent::DataReady.is_set_in(flags));
        assert!(DeviceEvent::SpaceAvailable.is_set_in(flags));

        // Flags should be cleared after check
        assert_eq!(manager.peek_poll(device_id), 0);
    }

    #[test]
    fn test_notification_manager_legacy_api() {
        let manager = DeviceNotificationManager::new(1);
        let device_id = DeviceId(1);
        let config = create_test_interrupt_config(32, 0);

        // Register using legacy API
        manager.register(device_id, config).unwrap();

        // Inject using legacy API
        manager.inject_irq(device_id, IrqType::Primary).unwrap();
        assert_eq!(manager.pending_count(0), 1);

        // Pop
        let pending = manager.pop_pending(0).unwrap();
        assert_eq!(pending.irq, 32);
    }

    #[test]
    fn test_notification_manager_transactional() {
        let manager = DeviceNotificationManager::new(1);
        let device_id = DeviceId(1);
        let config = create_test_notification_config(32, 0);

        manager.register_notification(device_id, config).unwrap();

        // Inject in pending state
        let (cpu_id, entry_id) = manager.inject_pending(device_id, IrqType::Primary).unwrap();
        assert_eq!(cpu_id, 0);
        assert_eq!(manager.pending_count(0), 0);
        assert_eq!(manager.uncommitted_count(0), 1);

        // Confirm
        manager.confirm_pending(cpu_id, entry_id);
        assert_eq!(manager.pending_count(0), 1);
        assert_eq!(manager.uncommitted_count(0), 0);
    }

    #[test]
    fn test_notification_manager_event() {
        let manager = DeviceNotificationManager::new(2);
        let device_id = DeviceId(1);
        let config = NotificationConfig::event()
            .with_cpu_affinity(CpuAffinity::Fixed(1));

        manager.register_notification(device_id, config).unwrap();

        // Inject events
        manager.inject(device_id, DeviceEvent::DataReady).unwrap();
        manager.inject(device_id, DeviceEvent::SpaceAvailable).unwrap();

        // Drain events
        let events = manager.drain_events(1, 10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, DeviceEvent::DataReady);
        assert_eq!(events[1].event, DeviceEvent::SpaceAvailable);
    }
}
