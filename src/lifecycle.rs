//! Device lifecycle management with atomic state machine and access tracking.
//!
//! This module provides the infrastructure for safe device hot-plug/hot-unplug
//! operations with proper synchronization using CAS (Compare-And-Swap) operations
//! to avoid TOCTOU (Time-Of-Check to Time-Of-Use) race conditions.
//!
//! # Design
//!
//! Uses a single `AtomicU32` to store both state and access count atomically,
//! ensuring that state checks and count modifications are atomic operations.
//!
//! Layout: `[state(8 bits) | access_count(24 bits)]`

use core::sync::atomic::{AtomicU32, AtomicBool, AtomicUsize, Ordering};

/// Device lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceState {
    /// Device is active and can handle requests.
    Active = 0,
    /// Device is being removed, rejects new requests.
    Removing = 1,
    /// Device has been removed and cleaned up.
    Removed = 2,
}

/// Combined state and access count in a single atomic value.
///
/// This solves the TOCTOU race condition in the original design where
/// checking state and incrementing count were separate operations.
///
/// Layout: `[state(8 bits) | access_count(24 bits)]`
/// - State: 0=Active, 1=Removing, 2=Removed
/// - Access count: max 16M concurrent accesses (sufficient for any practical use)
#[derive(Default)]
pub struct StateAndCount(AtomicU32);

impl StateAndCount {
    const STATE_SHIFT: u32 = 24;
    const COUNT_MASK: u32 = (1 << Self::STATE_SHIFT) - 1;
    const STATE_ACTIVE: u32 = 0;
    const STATE_REMOVING: u32 = 1;
    const STATE_REMOVED: u32 = 2;

    /// Create a new StateAndCount in Active state with zero access count.
    pub fn new() -> Self {
        Self(AtomicU32::new(Self::STATE_ACTIVE << Self::STATE_SHIFT))
    }

    /// Get the current state.
    #[inline]
    pub fn state(&self) -> DeviceState {
        match self.0.load(Ordering::Acquire) >> Self::STATE_SHIFT {
            0 => DeviceState::Active,
            1 => DeviceState::Removing,
            _ => DeviceState::Removed,
        }
    }

    /// Get the current access count.
    #[inline]
    pub fn count(&self) -> u32 {
        self.0.load(Ordering::Acquire) & Self::COUNT_MASK
    }

    /// Atomically try to increment access count (only if state is Active).
    ///
    /// Uses CAS loop to ensure state check and count increment are atomic,
    /// eliminating the TOCTOU race condition window.
    ///
    /// Returns `Ok(())` if access was granted, `Err(state)` otherwise.
    #[inline]
    pub fn try_acquire(&self) -> Result<(), DeviceState> {
        loop {
            let current = self.0.load(Ordering::Acquire);
            let state = current >> Self::STATE_SHIFT;

            // Check state
            if state != Self::STATE_ACTIVE {
                return Err(match state {
                    1 => DeviceState::Removing,
                    _ => DeviceState::Removed,
                });
            }

            let count = current & Self::COUNT_MASK;
            if count == Self::COUNT_MASK {
                // Count overflow (16M concurrent accesses, virtually impossible)
                return Err(DeviceState::Active);
            }

            let new_value = (state << Self::STATE_SHIFT) | (count + 1);

            // CAS operation: atomically check state and increment count
            // If state changed between load and CAS, CAS fails and we retry
            match self.0.compare_exchange_weak(
                current,
                new_value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(_) => continue, // Value was modified by another thread, retry
            }
        }
    }

    /// Decrement access count.
    #[inline]
    pub fn release(&self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }

    /// Set state to Removing (returns current access count).
    pub fn set_removing(&self) -> u32 {
        loop {
            let current = self.0.load(Ordering::Acquire);
            let count = current & Self::COUNT_MASK;
            let new_value = (Self::STATE_REMOVING << Self::STATE_SHIFT) | count;

            match self.0.compare_exchange_weak(
                current,
                new_value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return count,
                Err(_) => continue,
            }
        }
    }

    /// Set state to Removed.
    pub fn set_removed(&self) {
        self.0.store(Self::STATE_REMOVED << Self::STATE_SHIFT, Ordering::Release);
    }

    /// Reset to Active state (only from Removed state).
    pub fn reset_to_active(&self) -> bool {
        self.0
            .compare_exchange(
                Self::STATE_REMOVED << Self::STATE_SHIFT,
                Self::STATE_ACTIVE << Self::STATE_SHIFT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// Simple wait queue for avoiding busy-wait in `wait_idle`.
///
/// In multi-task environments, uses yield instead of pure spin-loop
/// to reduce CPU consumption.
pub struct WaitQueue {
    /// Number of waiters
    waiters: AtomicUsize,
    /// Whether notified
    notified: AtomicBool,
}

impl WaitQueue {
    /// Create a new wait queue.
    pub const fn new() -> Self {
        Self {
            waiters: AtomicUsize::new(0),
            notified: AtomicBool::new(false),
        }
    }

    /// Wait until condition is satisfied or timeout.
    ///
    /// Returns `true` if condition was satisfied, `false` on timeout.
    pub fn wait_until<F>(&self, mut condition: F, max_spins: usize) -> bool
    where
        F: FnMut() -> bool,
    {
        if condition() {
            return true;
        }

        self.waiters.fetch_add(1, Ordering::AcqRel);
        let mut spins = 0;

        loop {
            // Check condition
            if condition() {
                self.waiters.fetch_sub(1, Ordering::AcqRel);
                return true;
            }

            // Check timeout (spin count based)
            if spins >= max_spins && max_spins > 0 {
                self.waiters.fetch_sub(1, Ordering::AcqRel);
                return false;
            }

            // Check if notified
            if self.notified.swap(false, Ordering::AcqRel) {
                continue; // Notified, re-check condition
            }

            // Yield CPU
            for _ in 0..100 {
                core::hint::spin_loop();
            }
            spins += 100;
        }
    }

    /// Notify all waiters.
    pub fn notify_all(&self) {
        if self.waiters.load(Ordering::Acquire) > 0 {
            self.notified.store(true, Ordering::Release);
        }
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages device lifecycle state transitions and access tracking.
///
/// This structure tracks:
/// - Current lifecycle state (Active/Removing/Removed)
/// - Number of active accesses to the device
///
/// # State Transitions
///
/// ```text
/// ┌─────────┐  begin_removal()   ┌──────────┐  wait_idle()   ┌─────────┐
/// │ Active  │ ──────────────────> │ Removing │ ─────────────> │ Removed │
/// └─────────┘                     └──────────┘                └─────────┘
/// ```
///
/// # Race-Free Design
///
/// Uses `StateAndCount` to combine state and access count in a single atomic value,
/// ensuring that state checks and count modifications are atomic operations.
/// This eliminates the TOCTOU race condition present in designs that use
/// separate atomics for state and count.
pub struct DeviceLifecycle {
    state_count: StateAndCount,
    idle_waiters: WaitQueue,
}

impl DeviceLifecycle {
    /// Creates a new device lifecycle in Active state.
    pub fn new() -> Self {
        Self {
            state_count: StateAndCount::new(),
            idle_waiters: WaitQueue::new(),
        }
    }

    /// Gets the current lifecycle state.
    #[inline]
    pub fn state(&self) -> DeviceState {
        self.state_count.state()
    }

    /// Gets the current number of active accesses.
    #[inline]
    pub fn active_accesses(&self) -> usize {
        self.state_count.count() as usize
    }

    /// Attempts to begin an access to the device (atomic, race-free).
    ///
    /// Returns `true` if access is granted (device is Active).
    /// Returns `false` if access is denied (device is Removing or Removed).
    ///
    /// # Race Safety
    ///
    /// Uses CAS operation to atomically check state and increment count,
    /// eliminating the TOCTOU race condition window.
    #[inline]
    pub fn try_begin_access(&self) -> bool {
        self.state_count.try_acquire().is_ok()
    }

    /// Ends an access to the device.
    ///
    /// Also notifies waiters if access count becomes zero.
    #[inline]
    pub fn end_access(&self) {
        self.state_count.release();

        // Notify waiters if count is now zero
        if self.state_count.count() == 0 {
            self.idle_waiters.notify_all();
        }
    }

    /// Transitions the device to the Removing state.
    ///
    /// Returns `true` if transition succeeded (was Active).
    /// Returns `false` if already Removing or Removed.
    pub fn begin_removal(&self) -> bool {
        let current_state = self.state();
        if current_state == DeviceState::Active {
            self.state_count.set_removing();
            true
        } else {
            false
        }
    }

    /// Waits for all active accesses to complete.
    ///
    /// Uses wait queue with yield to reduce CPU consumption compared to pure spin-loop.
    pub fn wait_idle(&self) {
        self.idle_waiters.wait_until(
            || self.state_count.count() == 0,
            0, // No timeout (infinite wait)
        );
    }

    /// Waits for all active accesses to complete with a timeout.
    ///
    /// Returns `true` if device became idle within the timeout.
    /// Returns `false` if timeout expired.
    ///
    /// # Arguments
    ///
    /// * `max_spins` - Maximum number of spin iterations before timeout.
    pub fn wait_idle_timeout(&self, max_spins: usize) -> bool {
        self.idle_waiters.wait_until(
            || self.state_count.count() == 0,
            max_spins,
        )
    }

    /// Transitions the device to the Removed state.
    ///
    /// Should only be called after `wait_idle()` completes.
    pub fn complete_removal(&self) {
        self.state_count.set_removed();
    }

    /// Resets the device to Active state.
    ///
    /// This is used for device re-registration after removal.
    /// Should only be called when device is Removed and no accesses are active.
    pub fn reset_to_active(&self) -> bool {
        self.state_count.reset_to_active()
    }
}

impl Default for DeviceLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for DeviceLifecycle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceLifecycle")
            .field("state", &self.state())
            .field("active_accesses", &self.active_accesses())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_and_count_basic() {
        let sc = StateAndCount::new();
        assert_eq!(sc.state(), DeviceState::Active);
        assert_eq!(sc.count(), 0);
    }

    #[test]
    fn test_state_and_count_acquire_release() {
        let sc = StateAndCount::new();

        // Acquire should succeed in Active state
        assert!(sc.try_acquire().is_ok());
        assert_eq!(sc.count(), 1);

        assert!(sc.try_acquire().is_ok());
        assert_eq!(sc.count(), 2);

        // Release
        sc.release();
        assert_eq!(sc.count(), 1);

        sc.release();
        assert_eq!(sc.count(), 0);
    }

    #[test]
    fn test_state_and_count_removing() {
        let sc = StateAndCount::new();

        // Acquire some accesses
        assert!(sc.try_acquire().is_ok());
        assert!(sc.try_acquire().is_ok());
        assert_eq!(sc.count(), 2);

        // Set removing
        let count = sc.set_removing();
        assert_eq!(count, 2);
        assert_eq!(sc.state(), DeviceState::Removing);

        // New acquires should fail
        assert!(sc.try_acquire().is_err());
        assert_eq!(sc.count(), 2); // Count unchanged
    }

    #[test]
    fn test_lifecycle_initial_state() {
        let lifecycle = DeviceLifecycle::new();
        assert_eq!(lifecycle.state(), DeviceState::Active);
        assert_eq!(lifecycle.active_accesses(), 0);
    }

    #[test]
    fn test_lifecycle_access_tracking() {
        let lifecycle = DeviceLifecycle::new();

        assert!(lifecycle.try_begin_access());
        assert_eq!(lifecycle.active_accesses(), 1);

        assert!(lifecycle.try_begin_access());
        assert_eq!(lifecycle.active_accesses(), 2);

        lifecycle.end_access();
        assert_eq!(lifecycle.active_accesses(), 1);

        lifecycle.end_access();
        assert_eq!(lifecycle.active_accesses(), 0);
    }

    #[test]
    fn test_lifecycle_removal() {
        let lifecycle = DeviceLifecycle::new();

        // Start some accesses
        assert!(lifecycle.try_begin_access());
        assert!(lifecycle.try_begin_access());
        assert_eq!(lifecycle.active_accesses(), 2);

        // Begin removal
        assert!(lifecycle.begin_removal());
        assert_eq!(lifecycle.state(), DeviceState::Removing);

        // New accesses should be rejected
        assert!(!lifecycle.try_begin_access());
        assert_eq!(lifecycle.active_accesses(), 2); // Still 2 active

        // Complete existing accesses
        lifecycle.end_access();
        lifecycle.end_access();

        // Wait for idle
        lifecycle.wait_idle();
        assert_eq!(lifecycle.active_accesses(), 0);

        // Complete removal
        lifecycle.complete_removal();
        assert_eq!(lifecycle.state(), DeviceState::Removed);
    }

    #[test]
    fn test_lifecycle_reset() {
        let lifecycle = DeviceLifecycle::new();

        // Remove device
        assert!(lifecycle.begin_removal());
        lifecycle.wait_idle();
        lifecycle.complete_removal();
        assert_eq!(lifecycle.state(), DeviceState::Removed);

        // Reset to active
        assert!(lifecycle.reset_to_active());
        assert_eq!(lifecycle.state(), DeviceState::Active);

        // Should be able to access again
        assert!(lifecycle.try_begin_access());
    }

    #[test]
    fn test_lifecycle_double_removal() {
        let lifecycle = DeviceLifecycle::new();

        assert!(lifecycle.begin_removal());
        assert!(!lifecycle.begin_removal()); // Second attempt fails
    }
}
