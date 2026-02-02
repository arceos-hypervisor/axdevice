//! Zero-cost interior mutability abstraction for device state.
//!
//! `DeviceCell<T>` provides interior mutability with `Sync` semantics, safe because
//! device-level locks in the framework guarantee exclusive access during operations.
//!
//! # Safety Contract
//!
//! **This type is intended for use only within the AxDevice framework!**
//!
//! The safety of `DeviceCell` depends on the DeviceRegistry's device-level lock protection.
//! Using this type directly without going through the framework may cause data races.
//!
//! If you are implementing `BaseDeviceOps`, you can safely use this type because
//! the framework guarantees that `handle_read/write` calls are made while holding the device lock.

use core::cell::UnsafeCell;

/// A cell type that provides interior mutability with `Sync` semantics.
///
/// # Safety Contract
///
/// **This type is intended for use only within the AxDevice framework!**
///
/// The safety of `DeviceCell` relies on a two-level concurrency control mechanism:
///
/// 1. **Outer protection (DeviceRegistry)**:
///    - Each device has an independent `Mutex<()>` lock
///    - The framework **always acquires the device lock** before calling `handle_read/write`
///    - Ensures only one thread can access the same device at a time
///
/// 2. **Inner utility (DeviceCell)**:
///    - Due to the outer lock guarantee, `get_mut()` calls have no concurrent access
///    - Therefore, converting `&self` to `&mut T` is safe
///    - Implementing `Sync` is safe
///
/// # Why not RefCell?
///
/// `RefCell` does not implement the `Sync` trait and cannot be shared in a multi-threaded
/// environment:
///
/// ```rust,compile_fail
/// struct Device {
///     state: RefCell<State>,  // ❌ RefCell is not Sync
/// }
/// // Arc<dyn BaseDeviceOps> requires BaseDeviceOps: Sync
/// // Compilation fails!
/// ```
///
/// `DeviceCell` solves this problem:
///
/// ```rust,ignore
/// struct Device {
///     state: DeviceCell<State>,  // ✅ DeviceCell implements Sync
/// }
/// // Compilation succeeds! The framework's device lock ensures safety
/// ```
///
/// # Forbidden Usage
///
/// ```rust,compile_fail
/// // Do not use DeviceCell directly outside of handle_read/write!
/// let cell = DeviceCell::new(42);
/// std::thread::spawn(move || {
///     *cell.get_mut() = 100;  // ❌ Dangerous! No lock protection
/// });
/// ```
///
/// # Correct Usage
///
/// ```rust,ignore
/// use axdevice::DeviceCell;
///
/// struct MyDevice {
///     state: DeviceCell<DeviceState>,
/// }
///
/// impl BaseDeviceOps<GuestPhysAddrRange> for MyDevice {
///     fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
///         // ✅ Safe! Framework has already acquired device lock
///         let state = self.state.get_mut();
///         state.counter += 1;
///         Ok(state.counter)
///     }
/// }
/// ```
pub struct DeviceCell<T> {
    value: UnsafeCell<T>,
}

// SAFETY: DeviceCell is Sync because the device framework's locks guarantee
// exclusive access. Only one thread can access the device at a time through
// the DeviceRegistry's per-device Mutex.
unsafe impl<T: Send> Sync for DeviceCell<T> {}

impl<T> DeviceCell<T> {
    /// Creates a new `DeviceCell` containing the given value.
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
        }
    }

    /// Gets an immutable reference to the inner value.
    ///
    /// # Safety
    ///
    /// This is safe because the caller should hold the device lock, ensuring
    /// exclusive access.
    #[inline]
    pub fn get(&self) -> &T {
        // Safety: Caller must hold device lock (guaranteed by framework)
        unsafe { &*self.value.get() }
    }

    /// Gets a mutable reference to the inner value.
    ///
    /// # Safety
    ///
    /// This is safe because the caller should hold the device lock, ensuring
    /// exclusive access.
    #[inline]
    pub fn get_mut(&self) -> &mut T {
        // Safety: Caller must hold device lock (guaranteed by framework)
        unsafe { &mut *self.value.get() }
    }

    /// Replaces the contained value and returns the old value.
    #[inline]
    pub fn replace(&self, val: T) -> T {
        core::mem::replace(self.get_mut(), val)
    }

    /// Consumes the cell and returns the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

impl<T: Default> Default for DeviceCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Clone> Clone for DeviceCell<T> {
    fn clone(&self) -> Self {
        Self::new(self.get().clone())
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for DeviceCell<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.get().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_cell_basic() {
        let cell = DeviceCell::new(42);
        assert_eq!(*cell.get(), 42);
        *cell.get_mut() = 100;
        assert_eq!(*cell.get(), 100);
    }

    #[test]
    fn test_device_cell_replace() {
        let cell = DeviceCell::new(1);
        let old = cell.replace(2);
        assert_eq!(old, 1);
        assert_eq!(*cell.get(), 2);
    }

    #[test]
    fn test_device_cell_into_inner() {
        let cell = DeviceCell::new(42);
        assert_eq!(cell.into_inner(), 42);
    }

    #[test]
    fn test_device_cell_default() {
        let cell: DeviceCell<i32> = DeviceCell::default();
        assert_eq!(*cell.get(), 0);
    }

    #[test]
    fn test_device_cell_clone() {
        let cell1 = DeviceCell::new(42);
        let cell2 = cell1.clone();
        assert_eq!(*cell2.get(), 42);
        *cell1.get_mut() = 100;
        assert_eq!(*cell2.get(), 42); // Clones are independent
    }
}
