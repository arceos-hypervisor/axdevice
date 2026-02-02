//! Block backend implementation using axdriver's AxBlockDevice.
//!
//! This module provides a `BlockBackend` implementation that wraps axdriver's
//! `AxBlockDevice`, enabling VirtIO block devices to use host block devices.

use alloc::sync::Arc;
use spin::Mutex;

use axvirtio_blk::BlockBackend;
use axvirtio_common::VirtioResult;

use axdriver_block::BlockDriverOps;

/// A block backend that wraps axdriver's AxBlockDevice.
///
/// This struct implements the `BlockBackend` trait for axvirtio-blk,
/// allowing the hypervisor to use host block devices as backing storage
/// for virtual VirtIO block devices presented to guest VMs.
pub struct AxBlockBackend<D: BlockDriverOps> {
    /// The underlying block device.
    device: Arc<Mutex<D>>,
    /// Block size in bytes.
    block_size: usize,
    /// Total number of blocks.
    num_blocks: u64,
}

impl<D: BlockDriverOps> AxBlockBackend<D> {
    /// Create a new AxBlockBackend wrapping the given device.
    pub fn new(device: D) -> Self {
        let block_size = device.block_size();
        let num_blocks = device.num_blocks();
        Self {
            device: Arc::new(Mutex::new(device)),
            block_size,
            num_blocks,
        }
    }

    /// Get the block size in bytes.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Get the total number of blocks.
    pub fn num_blocks(&self) -> u64 {
        self.num_blocks
    }

    /// Get the total capacity in bytes.
    pub fn capacity(&self) -> u64 {
        self.num_blocks * self.block_size as u64
    }
}

impl<D: BlockDriverOps + Send + Sync + 'static> BlockBackend for AxBlockBackend<D> {
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        let mut device = self.device.lock();

        // VirtIO uses 512-byte sectors
        const VIRTIO_SECTOR_SIZE: usize = 512;

        // Calculate actual block ID based on device block size
        let bytes_offset = sector * VIRTIO_SECTOR_SIZE as u64;
        let block_id = bytes_offset / self.block_size as u64;
        let offset_in_block = (bytes_offset % self.block_size as u64) as usize;

        // Read data
        let mut total_read = 0;
        let mut current_block = block_id;
        let mut current_offset = offset_in_block;
        let mut remaining = buffer.len();

        while remaining > 0 {
            // Read one block
            let mut block_buf = alloc::vec![0u8; self.block_size];
            device.read_block(current_block, &mut block_buf)
                .map_err(|_| axvirtio_common::VirtioError::BackendError)?;

            // Copy data from block buffer
            let copy_len = (self.block_size - current_offset).min(remaining);
            buffer[total_read..total_read + copy_len]
                .copy_from_slice(&block_buf[current_offset..current_offset + copy_len]);

            total_read += copy_len;
            remaining -= copy_len;
            current_block += 1;
            current_offset = 0;
        }

        Ok(total_read)
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        let mut device = self.device.lock();

        // VirtIO uses 512-byte sectors
        const VIRTIO_SECTOR_SIZE: usize = 512;

        // Calculate actual block ID based on device block size
        let bytes_offset = sector * VIRTIO_SECTOR_SIZE as u64;
        let block_id = bytes_offset / self.block_size as u64;
        let offset_in_block = (bytes_offset % self.block_size as u64) as usize;

        // Write data
        let mut total_written = 0;
        let mut current_block = block_id;
        let mut current_offset = offset_in_block;
        let mut remaining = buffer.len();

        while remaining > 0 {
            let copy_len = (self.block_size - current_offset).min(remaining);

            // If not writing a full block, read-modify-write
            let mut block_buf = alloc::vec![0u8; self.block_size];
            if copy_len < self.block_size || current_offset > 0 {
                device.read_block(current_block, &mut block_buf)
                    .map_err(|_| axvirtio_common::VirtioError::BackendError)?;
            }

            // Copy data to block buffer
            block_buf[current_offset..current_offset + copy_len]
                .copy_from_slice(&buffer[total_written..total_written + copy_len]);

            // Write block
            device.write_block(current_block, &block_buf)
                .map_err(|_| axvirtio_common::VirtioError::BackendError)?;

            total_written += copy_len;
            remaining -= copy_len;
            current_block += 1;
            current_offset = 0;
        }

        Ok(total_written)
    }

    fn flush(&self) -> VirtioResult<()> {
        let mut device = self.device.lock();
        device.flush()
            .map_err(|_| axvirtio_common::VirtioError::BackendError)
    }
}

// Safety: AxBlockBackend is safe to send and share between threads because
// it uses Arc<Mutex<...>> internally.
unsafe impl<D: BlockDriverOps + Send> Send for AxBlockBackend<D> {}
unsafe impl<D: BlockDriverOps + Send> Sync for AxBlockBackend<D> {}
