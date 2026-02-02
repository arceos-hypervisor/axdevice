//! Notification management system for device emulation.
//!
//! This module provides a unified notification layer that decouples devices
//! from architecture-specific notification mechanisms.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐
//! │   Device    │ Declares notification config via `notification_config()`
//! └──────┬──────┘
//!        │
//!        ▼
//! ┌─────────────────────┐
//! │ DeviceNotifier      │ Injected via `set_notifier()`
//! │ (DeviceNotifierImpl)│
//! └──────┬──────────────┘
//!        │ notify(DeviceEvent)
//!        ▼
//! ┌────────────────────────┐
//! │ DeviceNotificationManager│ Routes and queues notifications
//! │  - RoutingTable        │ (supports Interrupt/Poll/Event)
//! │  - PriorityQueues      │
//! │  - PollFlags           │
//! │  - EventQueues         │
//! └──────┬─────────────────┘
//!        │
//!        ▼
//! ┌─────────────┐
//! │  vCPU/VM    │ Pops and injects interrupts / checks poll flags
//! └─────────────┘
//! ```
//!
//! # Design Goals
//!
//! 1. **Decoupling**: Devices don't depend on architecture-specific notification mechanisms
//! 2. **Flexibility**: Supports multiple notification methods (Interrupt, Poll, Callback, Event)
//! 3. **Backward Compatibility**: Existing devices using `InterruptTrigger` continue to work
//! 4. **Performance**: Efficient routing and queuing with minimal overhead
//! 5. **Testability**: Devices can be tested with mock notifiers
//!
//! # Notification Methods
//!
//! - **Interrupt**: Traditional hardware interrupt via vPLIC/vGIC injection
//! - **Poll**: Device sets atomic flag, vCPU loop checks periodically (low-latency)
//! - **Callback**: Synchronous callback execution (testing/simple scenarios)
//! - **Event**: Asynchronous event queue supporting batch processing
//!
//! # Usage Example
//!
//! ```rust,ignore
//! use axdevice::notify::DeviceNotificationManager;
//! use axdevice_base::{NotificationConfig, NotifyMethod, DeviceEvent};
//!
//! // Create notification manager for 4 vCPUs
//! let manager = Arc::new(DeviceNotificationManager::new(4));
//!
//! // Register device notification
//! let device_id = DeviceId(1);
//! let config = NotificationConfig::interrupt(32)
//!     .with_priority(100)
//!     .with_coalesce(true);
//! manager.register(device_id, config)?;
//!
//! // Create and inject notifier into device
//! let notifier = Arc::new(DeviceNotifierImpl::new(device_id, Arc::clone(&manager)));
//! device.set_notifier(notifier);
//!
//! // Device sends notification
//! notifier.notify(DeviceEvent::DataReady)?;
//!
//! // VM pops pending interrupt before entry (for Interrupt method)
//! if let Some(pending) = manager.pop_pending(cpu_id) {
//!     vcpu.inject_interrupt(pending.irq);
//! }
//!
//! // Or check poll flags (for Poll method)
//! let flags = manager.check_poll(device_id);
//! if DeviceEvent::DataReady.is_set_in(flags) {
//!     // Handle data ready...
//! }
//! ```

mod manager;
mod notifier;
mod queue;
mod routing;
mod poll;

pub use manager::DeviceNotificationManager;
pub use notifier::DeviceNotifierImpl;
pub use queue::PendingNotification;
pub use poll::PollFlags;

// Re-export legacy types for backward compatibility
pub use manager::DeviceInterruptManager;
pub use notifier::DeviceInterruptTrigger;
pub use queue::PendingInterrupt;
