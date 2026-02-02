//! Console backend implementation using axplat's console interface.
//!
//! This module provides a `ConsoleBackend` implementation that wraps axplat's
//! console interface, enabling VirtIO console devices to use the host UART.

use axvirtio_console::ConsoleBackend;
use axvirtio_common::VirtioResult;

/// A console backend that uses the platform's console interface.
///
/// This struct implements the `ConsoleBackend` trait for axvirtio-console,
/// allowing the hypervisor to use the host UART as backing I/O
/// for virtual VirtIO console devices presented to guest VMs.
///
/// # Note
///
/// This backend uses `axplat::console` which provides a platform-independent
/// interface to the console (typically UART on RISC-V QEMU).
///
/// # Interrupt-Driven Design
///
/// When UART receives data, it triggers an interrupt. The hypervisor's IRQ
/// handler sets a flag, and the vCPU loop calls `poll_console_input()` which
/// eventually calls `backend.read()`. This design provides low-latency input
/// handling without polling overhead.
pub struct AxConsoleBackend {
    /// Terminal columns
    cols: u16,
    /// Terminal rows
    rows: u16,
}

impl Default for AxConsoleBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AxConsoleBackend {
    /// Create a new AxConsoleBackend using the platform console.
    pub fn new() -> Self {
        Self {
            cols: 80,
            rows: 25,
        }
    }

    /// Create a new AxConsoleBackend with specified terminal size.
    pub fn with_size(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

impl ConsoleBackend for AxConsoleBackend {
    fn read(&self, buffer: &mut [u8]) -> VirtioResult<usize> {
        // Directly read from UART - interrupt-driven, data should be available
        let count = axplat::console::read_bytes(buffer);
        if count > 0 {
            trace!("[AxConsoleBackend] Read {} bytes from UART", count);
        }
        Ok(count)
    }

    fn write(&self, buffer: &[u8]) -> VirtioResult<usize> {
        // Use axplat's console write interface
        axplat::console::write_bytes(buffer);
        Ok(buffer.len())
    }

    fn has_pending_input(&self) -> bool {
        // In interrupt-driven mode, this is called because UART IRQ fired.
        // We assume data is available - the actual read() will confirm.
        //
        // Note: We cannot peek UART without consuming data, so we return true
        // optimistically. If no data is actually available, read() will return 0.
        true
    }

    fn get_size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
}

// Safety: AxConsoleBackend is safe to send and share between threads because
// it only holds simple configuration data and uses the platform console
// interface which is designed to be thread-safe.
unsafe impl Send for AxConsoleBackend {}
unsafe impl Sync for AxConsoleBackend {}
