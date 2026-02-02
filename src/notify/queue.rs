//! Notification priority queue for pending notifications.
//!
//! This module provides both a simple priority queue and a transactional queue
//! that supports two-phase commit for atomic notification injection.
//!
//! ## Types
//!
//! - `PendingNotification`: New notification type with `DeviceEvent` support
//! - `PendingInterrupt`: Legacy type alias for backward compatibility

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::collections::BinaryHeap;
use core::cmp::Ordering;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use axdevice_base::DeviceEvent;
use crate::wrapper::DeviceId;

/// A pending notification in the priority queue.
///
/// Notifications are ordered by priority (higher first), then by timestamp (earlier first).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PendingNotification {
    /// The IRQ number to inject (for interrupt-based notifications).
    pub irq: u32,
    /// Notification priority (0-255, higher is more important).
    pub priority: u8,
    /// The device that triggered this notification.
    pub device_id: DeviceId,
    /// Timestamp when the notification was triggered (for ordering).
    pub timestamp: u64,
    /// The event that triggered this notification.
    pub event: DeviceEvent,
}

impl PendingNotification {
    /// Creates a new pending notification.
    pub fn new(irq: u32, priority: u8, device_id: DeviceId, timestamp: u64) -> Self {
        Self {
            irq,
            priority,
            device_id,
            timestamp,
            event: DeviceEvent::Irq(axdevice_base::IrqType::Primary),
        }
    }

    /// Creates a new pending notification with a specific event.
    pub fn with_event(irq: u32, priority: u8, device_id: DeviceId, timestamp: u64, event: DeviceEvent) -> Self {
        Self {
            irq,
            priority,
            device_id,
            timestamp,
            event,
        }
    }
}

impl Ord for PendingNotification {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max heap, so we want higher priority to be "greater"
        // 1. Compare priorities: higher priority should be greater
        match self.priority.cmp(&other.priority) {
            Ordering::Equal => {
                // 2. For same priority, earlier timestamp should be greater (pop first)
                //    So we reverse the comparison
                other.timestamp.cmp(&self.timestamp)
            }
            priority_ordering => priority_ordering,
        }
    }
}

impl PartialOrd for PendingNotification {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Legacy type alias for backward compatibility.
pub type PendingInterrupt = PendingNotification;

/// Entry ID for transactional operations.
pub type EntryId = u64;

/// Transactional notification queue supporting two-phase commit.
///
/// This queue solves the notification injection order problem: if we add a notification
/// to the queue and then notify the hardware, but hardware notification fails,
/// we need to rollback the queue entry.
///
/// # Usage
///
/// ```rust,ignore
/// let mut queue = TransactionalNotificationQueue::new();
///
/// // Phase 1: Add to queue (pending state)
/// let entry_id = queue.push_pending(notification);
///
/// // Phase 2: Try to notify hardware
/// match arch_controller.inject(cpu, irq) {
///     Ok(()) => queue.confirm(entry_id),    // Success: confirm
///     Err(e) => queue.rollback(entry_id),   // Failed: rollback
/// }
/// ```
pub struct TransactionalNotificationQueue {
    /// Confirmed notifications (priority queue).
    confirmed: BinaryHeap<PendingNotification>,
    /// Pending (uncommitted) notifications.
    pending: BTreeMap<EntryId, PendingNotification>,
    /// Next entry ID.
    next_entry_id: AtomicU64,
}

impl TransactionalNotificationQueue {
    /// Create a new transactional notification queue.
    pub fn new() -> Self {
        Self {
            confirmed: BinaryHeap::new(),
            pending: BTreeMap::new(),
            next_entry_id: AtomicU64::new(0),
        }
    }

    /// Add a pending (uncommitted) notification entry.
    ///
    /// Returns an entry ID that can be used to confirm or rollback.
    pub fn push_pending(&mut self, notification: PendingNotification) -> EntryId {
        let id = self.next_entry_id.fetch_add(1, AtomicOrdering::Relaxed);
        self.pending.insert(id, notification);
        id
    }

    /// Confirm a pending entry (move to confirmed queue).
    ///
    /// Call this after hardware notification succeeds.
    pub fn confirm(&mut self, entry_id: EntryId) {
        if let Some(notification) = self.pending.remove(&entry_id) {
            self.confirmed.push(notification);
        }
    }

    /// Rollback a pending entry (remove from queue).
    ///
    /// Call this if hardware notification fails.
    pub fn rollback(&mut self, entry_id: EntryId) {
        self.pending.remove(&entry_id);
    }

    /// Push a notification directly to confirmed queue (non-transactional).
    ///
    /// Use this when transactional semantics are not needed.
    pub fn push(&mut self, notification: PendingNotification) {
        self.confirmed.push(notification);
    }

    /// Pop the highest priority confirmed notification.
    pub fn pop(&mut self) -> Option<PendingNotification> {
        self.confirmed.pop()
    }

    /// Peek at the highest priority confirmed notification without removing it.
    pub fn peek(&self) -> Option<&PendingNotification> {
        self.confirmed.peek()
    }

    /// Get the number of confirmed notifications.
    pub fn len(&self) -> usize {
        self.confirmed.len()
    }

    /// Check if the confirmed queue is empty.
    pub fn is_empty(&self) -> bool {
        self.confirmed.is_empty()
    }

    /// Get the number of pending (uncommitted) notifications.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Clear all confirmed notifications.
    pub fn clear(&mut self) {
        self.confirmed.clear();
    }

    /// Clear all pending (uncommitted) notifications.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }

    /// Clear everything (both confirmed and pending).
    pub fn clear_all(&mut self) {
        self.confirmed.clear();
        self.pending.clear();
    }
}

impl Default for TransactionalNotificationQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Legacy type alias for backward compatibility.
pub type TransactionalInterruptQueue = TransactionalNotificationQueue;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_notification_ordering() {
        let n1 = PendingNotification::new(32, 100, DeviceId(1), 1000);
        let n2 = PendingNotification::new(33, 200, DeviceId(2), 1001);
        let n3 = PendingNotification::new(34, 100, DeviceId(3), 999);

        // Higher priority should come first
        assert!(n2 > n1);
        assert!(n2 > n3);

        // Same priority: earlier timestamp comes first
        assert!(n3 > n1);
    }

    #[test]
    fn test_pending_notification_with_event() {
        let n = PendingNotification::with_event(
            32, 100, DeviceId(1), 1000,
            DeviceEvent::DataReady
        );
        assert_eq!(n.event, DeviceEvent::DataReady);
    }

    #[test]
    fn test_transactional_queue_confirm() {
        let mut queue = TransactionalNotificationQueue::new();

        let n = PendingNotification::new(32, 100, DeviceId(1), 1000);

        // Push as pending
        let entry_id = queue.push_pending(n);
        assert_eq!(queue.len(), 0); // Not in confirmed yet
        assert_eq!(queue.pending_count(), 1);

        // Confirm
        queue.confirm(entry_id);
        assert_eq!(queue.len(), 1); // Now in confirmed
        assert_eq!(queue.pending_count(), 0);

        // Pop
        let popped = queue.pop().unwrap();
        assert_eq!(popped.irq, 32);
    }

    #[test]
    fn test_transactional_queue_rollback() {
        let mut queue = TransactionalNotificationQueue::new();

        let n = PendingNotification::new(32, 100, DeviceId(1), 1000);

        // Push as pending
        let entry_id = queue.push_pending(n);
        assert_eq!(queue.pending_count(), 1);

        // Rollback
        queue.rollback(entry_id);
        assert_eq!(queue.pending_count(), 0);
        assert_eq!(queue.len(), 0); // Nothing in confirmed either
    }

    #[test]
    fn test_transactional_queue_priority() {
        let mut queue = TransactionalNotificationQueue::new();

        // Add notifications with different priorities
        queue.push(PendingNotification::new(32, 100, DeviceId(1), 1000));
        queue.push(PendingNotification::new(33, 200, DeviceId(2), 1001));
        queue.push(PendingNotification::new(34, 50, DeviceId(3), 999));

        // Should pop in priority order (highest first)
        assert_eq!(queue.pop().unwrap().irq, 33); // priority 200
        assert_eq!(queue.pop().unwrap().irq, 32); // priority 100
        assert_eq!(queue.pop().unwrap().irq, 34); // priority 50
    }
}
