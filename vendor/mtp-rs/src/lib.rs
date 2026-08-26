//! # mtp-rs
//!
//! A pure-Rust MTP (Media Transfer Protocol) library targeting modern Android devices.
//!
//! ## Features
//!
//! - **Runtime agnostic**: Uses `futures` traits, works with any async runtime
//! - **Two-level API**: High-level `mtp::` for media devices, low-level `ptp::` for cameras
//! - **Streaming**: Memory-efficient streaming downloads with progress tracking
//! - **Type safe**: Newtype wrappers prevent mixing up IDs
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use mtp_rs::mtp::MtpDevice;
//!
//! # async fn example() -> Result<(), mtp_rs::Error> {
//! // Open the first MTP device
//! let device = MtpDevice::open_first().await?;
//!
//! println!("Connected to: {} {}",
//!          device.device_info().manufacturer,
//!          device.device_info().model);
//!
//! // Get storages
//! for storage in device.storages().await? {
//!     println!("Storage: {} ({} free)",
//!              storage.info().description,
//!              storage.info().free_space_bytes);
//!
//!     // List root folder
//!     for obj in storage.list_objects(None).await? {
//!         let kind = if obj.is_folder() { "DIR " } else { "FILE" };
//!         println!("  {} {} ({} bytes)", kind, obj.filename, obj.size);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod mtp;
pub mod ptp;
pub mod transport;

pub use error::Error;

// Re-export core types for convenience
pub use ptp::{
    receive_stream_to_stream, DateTime, EventCode, ObjectFormatCode, ObjectHandle, OperationCode,
    ReceiveStream, ResponseCode, SessionId, StorageId, TransactionId,
};

// Re-export high-level MTP types
pub use mtp::{
    DeviceEvent, FileDownload, MtpDevice, MtpDeviceBuilder, NewObjectInfo, ObjectListing, Progress,
    Storage, DEFAULT_CANCEL_TIMEOUT,
};

// Re-export virtual device types when the feature is enabled
#[cfg(feature = "virtual-device")]
pub use transport::virtual_device::config::{VirtualDeviceConfig, VirtualStorageConfig};
#[cfg(feature = "virtual-device")]
pub use transport::virtual_device::registry::{
    pause_watcher, register_virtual_device, rescan_virtual_device, unregister_virtual_device,
    WatcherGuard,
};
#[cfg(feature = "virtual-device")]
pub use transport::virtual_device::RescanSummary;
