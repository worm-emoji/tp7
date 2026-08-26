//! Virtual MTP device transport for testing.
//!
//! This module provides a [`VirtualTransport`] that implements the [`Transport`] trait
//! using a local filesystem directory as its backing store. It speaks the full MTP/PTP
//! binary protocol, so the existing `PtpSession`, `MtpDevice`, and `Storage` types
//! work unchanged.
//!
//! Use this to test MTP client code without real USB hardware.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::path::PathBuf;
//! use std::time::Duration;
//! use mtp_rs::MtpDevice;
//! use mtp_rs::transport::virtual_device::config::{VirtualDeviceConfig, VirtualStorageConfig};
//!
//! # async fn example() -> Result<(), mtp_rs::Error> {
//! let device = MtpDevice::builder()
//!     .open_virtual(VirtualDeviceConfig {
//!         manufacturer: "Google".into(),
//!         model: "Virtual Pixel 9".into(),
//!         serial: "virtual-001".into(),
//!         storages: vec![VirtualStorageConfig {
//!             description: "Internal Storage".into(),
//!             capacity: 64 * 1024 * 1024 * 1024,
//!             backing_dir: PathBuf::from("/tmp/mtp-test"),
//!             read_only: false,
//!         }],
//!         supports_rename: true,
//!         event_poll_interval: Duration::from_millis(50),
//!         watch_backing_dirs: true,
//!     })
//!     .await?;
//!
//! // Use the device exactly like a real one
//! for storage in device.storages().await? {
//!     for obj in storage.list_objects(None).await? {
//!         println!("{}", obj.filename);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod builders;
pub mod config;
mod handlers;
pub mod registry;
mod state;
mod watcher;

use crate::ptp::{unpack_u16, unpack_u32};
use crate::transport::Transport;
use async_trait::async_trait;
use config::VirtualDeviceConfig;
pub use state::RescanSummary;
use state::{PendingCommand, VirtualDeviceState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A transport that speaks MTP/PTP binary protocol against a local filesystem.
///
/// Created via `MtpDeviceBuilder::open_virtual()` or directly for lower-level use.
///
/// Internally, incoming `send_bulk` calls are parsed as MTP command/data containers.
/// The virtual device processes each operation (list files, read, write, delete, etc.)
/// against the configured backing directories and queues binary response containers
/// for the next `receive_bulk` call.
///
/// A background filesystem watcher detects out-of-band changes to the backing
/// directories and queues corresponding MTP events. The watcher is stopped
/// automatically when the transport is dropped.
pub struct VirtualTransport {
    state: Arc<Mutex<VirtualDeviceState>>,
    /// How long `receive_interrupt` waits when no events are pending.
    event_poll_interval: Duration,
    /// Serial number, used to unregister from the active-states registry on drop.
    serial: String,
    /// Filesystem watcher. Stops watching when dropped.
    _watcher: Option<notify::RecommendedWatcher>,
}

impl VirtualTransport {
    /// Create a new virtual transport from a device configuration.
    ///
    /// The backing directories in each storage config should already exist.
    /// When `config.watch_backing_dirs` is `true`, starts a background
    /// filesystem watcher for detecting out-of-band changes.
    #[must_use]
    pub fn new(config: VirtualDeviceConfig) -> Self {
        let event_poll_interval = config.event_poll_interval;
        let watch = config.watch_backing_dirs;
        let serial = config.serial.clone();
        let state = Arc::new(Mutex::new(VirtualDeviceState::new(config)));

        // Register the state so `rescan_virtual_device()` can find it.
        registry::register_active_state(serial.clone(), Arc::clone(&state));

        let watcher = if watch {
            watcher::start_fs_watcher(&state)
        } else {
            None
        };
        Self {
            state,
            event_poll_interval,
            serial,
            _watcher: watcher,
        }
    }
}

impl Drop for VirtualTransport {
    fn drop(&mut self) {
        registry::unregister_active_state(&self.serial);
    }
}

/// Container type constants.
const CONTAINER_TYPE_COMMAND: u16 = 1;
const CONTAINER_TYPE_DATA: u16 = 2;

#[async_trait]
impl Transport for VirtualTransport {
    async fn send_bulk(&self, data: &[u8]) -> Result<(), crate::Error> {
        if data.len() < 12 {
            return Err(crate::Error::invalid_data("container too small"));
        }

        let _length = unpack_u32(&data[0..4])?;
        let container_type = unpack_u16(&data[4..6])?;
        let code = unpack_u16(&data[6..8])?;
        let tx_id = unpack_u32(&data[8..12])?;

        let mut state = self.state.lock().unwrap();

        match container_type {
            CONTAINER_TYPE_COMMAND => {
                // Parse parameters (each u32, after the 12-byte header)
                let param_bytes = data.len() - 12;
                let param_count = param_bytes / 4;
                let mut params = Vec::with_capacity(param_count);
                for i in 0..param_count {
                    let offset = 12 + i * 4;
                    params.push(unpack_u32(&data[offset..])?);
                }

                // Check if this operation expects a data phase from the host.
                // If so, don't dispatch yet -- store the command and wait for data.
                let op = crate::ptp::OperationCode::from(code);
                if matches!(
                    op,
                    crate::ptp::OperationCode::SendObjectInfo
                        | crate::ptp::OperationCode::SendObject
                        | crate::ptp::OperationCode::SetObjectPropValue
                ) {
                    state.pending_command = Some(PendingCommand {
                        code,
                        tx_id,
                        params,
                    });
                } else {
                    handlers::dispatch(&mut state, code, tx_id, &params, None);
                }
            }
            CONTAINER_TYPE_DATA => {
                // This is the data phase for a previous command.
                match state.pending_command.take() {
                    Some(pending) => {
                        let payload = &data[12..]; // Skip data container header
                        handlers::dispatch(
                            &mut state,
                            pending.code,
                            pending.tx_id,
                            &pending.params,
                            Some(payload),
                        );
                    }
                    None => {
                        return Err(crate::Error::invalid_data(
                            "received data container without pending command",
                        ));
                    }
                }
            }
            _ => {
                return Err(crate::Error::invalid_data(format!(
                    "unexpected container type: {}",
                    container_type
                )));
            }
        }

        Ok(())
    }

    async fn receive_bulk(&self, _max_size: usize) -> Result<Vec<u8>, crate::Error> {
        let mut state = self.state.lock().unwrap();
        match state.response_queue.pop_front() {
            Some(data) => Ok(data),
            None => Err(crate::Error::invalid_data("no response available")),
        }
    }

    async fn receive_interrupt(&self) -> Result<Vec<u8>, crate::Error> {
        // Check for events (drop lock before any await)
        {
            let mut state = self.state.lock().unwrap();
            if let Some(event) = state.event_queue.pop_front() {
                return Ok(event);
            }
        }
        // No events — wait, then return Timeout
        futures_timer::Delay::new(self.event_poll_interval).await;
        Err(crate::Error::Timeout)
    }

    async fn cancel_transfer(
        &self,
        _transaction_id: u32,
        _idle_timeout: std::time::Duration,
    ) -> Result<(), crate::Error> {
        // Virtual device has no USB pipe to drain — just clear any pending state.
        let mut state = self.state.lock().unwrap();
        state.pending_command = None;
        state.response_queue.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::config::{VirtualDeviceConfig, VirtualStorageConfig};
    use crate::mtp::MtpDevice;
    use crate::ptp::ObjectFormatCode;
    use std::time::Duration;

    fn test_config(dir: &std::path::Path) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: "test-001".into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024, // 1 GB
                backing_dir: dir.to_path_buf(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        }
    }

    fn test_config_readonly(dir: &std::path::Path) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: "test-ro".into(),
            storages: vec![VirtualStorageConfig {
                description: "Read-only Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: dir.to_path_buf(),
                read_only: true,
            }],
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        }
    }

    fn test_config_multi(dirs: &[&std::path::Path]) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: "test-multi".into(),
            storages: dirs
                .iter()
                .enumerate()
                .map(|(i, d)| VirtualStorageConfig {
                    description: format!("Storage {}", i + 1),
                    capacity: 1024 * 1024 * 1024,
                    backing_dir: d.to_path_buf(),
                    read_only: false,
                })
                .collect(),
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        }
    }

    /// Helper to convert `&[u8]` to a `Stream<Item = Result<Bytes, io::Error>>`.
    fn bytes_stream(
        data: &[u8],
    ) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin {
        futures::stream::once(futures::future::ok(bytes::Bytes::copy_from_slice(data)))
    }

    #[tokio::test]
    async fn open_virtual_and_list_storages() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        assert_eq!(storages.len(), 1);
        assert_eq!(storages[0].info().description, "Internal Storage");
        assert!(storages[0].info().max_capacity > 0);
    }

    #[tokio::test]
    async fn device_info_matches_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let info = device.device_info();

        assert_eq!(info.manufacturer, "TestCorp");
        assert_eq!(info.model, "Virtual Phone");
        assert_eq!(info.serial_number, "test-001");
        assert!(device.supports_rename());
    }

    #[tokio::test]
    async fn list_objects_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let objects = storages[0].list_objects(None).await.unwrap();

        assert!(objects.is_empty());
    }

    #[tokio::test]
    async fn list_objects_with_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
        std::fs::write(dir.path().join("photo.jpg"), "fake jpeg data").unwrap();
        std::fs::create_dir(dir.path().join("Documents")).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let items = storages[0].list_objects(None).await.unwrap();

        assert_eq!(items.len(), 3);
        let names: Vec<&str> = items.iter().map(|i| i.filename.as_str()).collect();
        assert!(names.contains(&"hello.txt"));
        assert!(names.contains(&"photo.jpg"));
        assert!(names.contains(&"Documents"));

        // Verify folder detection
        let docs = items.iter().find(|i| i.filename == "Documents").unwrap();
        assert!(docs.is_folder());
        assert_eq!(docs.format, ObjectFormatCode::Association);

        // Verify file metadata
        let txt = items.iter().find(|i| i.filename == "hello.txt").unwrap();
        assert!(txt.is_file());
        assert_eq!(txt.size, 11); // "hello world" = 11 bytes
    }

    #[tokio::test]
    async fn download_file() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"test file content for download";
        std::fs::write(dir.path().join("test.txt"), content).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let items = storages[0].list_objects(None).await.unwrap();
        let obj = &items[0];

        let data = storages[0].download(obj.handle).await.unwrap();
        assert_eq!(data.as_slice(), content);
    }

    #[tokio::test]
    async fn download_partial_reads_byte_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        std::fs::write(dir.path().join("data.bin"), &content).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let items = storages[0].list_objects(None).await.unwrap();
        let obj = &items[0];

        // Read from the beginning.
        let head = storages[0]
            .download_partial(obj.handle, 0, 100)
            .await
            .unwrap();
        assert_eq!(head, &content[0..100]);

        // Read from the middle.
        let middle = storages[0]
            .download_partial(obj.handle, 500, 100)
            .await
            .unwrap();
        assert_eq!(middle, &content[500..600]);

        // Read past the end: device returns whatever's left.
        let tail = storages[0]
            .download_partial(obj.handle, 990, 1000)
            .await
            .unwrap();
        assert_eq!(tail, &content[990..1000]);
    }

    #[tokio::test]
    async fn download_partial_64_reads_byte_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        std::fs::write(dir.path().join("data.bin"), &content).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let items = storages[0].list_objects(None).await.unwrap();
        let obj = &items[0];

        // Same scenarios as the 32-bit version, using the 64-bit op.
        let head = storages[0]
            .download_partial_64(obj.handle, 0, 100)
            .await
            .unwrap();
        assert_eq!(head, &content[0..100]);

        let middle = storages[0]
            .download_partial_64(obj.handle, 500, 100)
            .await
            .unwrap();
        assert_eq!(middle, &content[500..600]);

        let tail = storages[0]
            .download_partial_64(obj.handle, 990, 1000)
            .await
            .unwrap();
        assert_eq!(tail, &content[990..1000]);
    }

    #[tokio::test]
    async fn download_partial_64_reassembles_offset_correctly() {
        // Verifies the lo/hi u32 → u64 round-trip. We can't realistically create a >4GB
        // file, so instead we test that an offset whose low u32 is 0 (i.e. an exact
        // multiple of 2^32) routes through the same code path with no truncation.
        // For a small file, any offset >= file length returns an empty read, which
        // still proves the u64 offset made it through correctly (if the high bits
        // were dropped, we'd incorrectly read from the start of the file).
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..100).map(|i| i as u8).collect();
        std::fs::write(dir.path().join("small.bin"), &content).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let items = storages[0].list_objects(None).await.unwrap();
        let obj = &items[0];

        // Offset = 2^32 + 10. If the hi u32 were dropped, this would read from byte 10.
        let offset_beyond_4gb: u64 = (1u64 << 32) + 10;
        let data = storages[0]
            .download_partial_64(obj.handle, offset_beyond_4gb, 50)
            .await
            .unwrap();
        assert!(
            data.is_empty(),
            "offset past EOF should return empty, got {} bytes — high offset bits may be getting dropped",
            data.len()
        );
    }

    #[tokio::test]
    async fn upload_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let info = crate::mtp::NewObjectInfo::file("uploaded.txt", 12);
        let handle = storages[0]
            .upload(None, info, bytes_stream(b"hello upload"))
            .await
            .unwrap();

        // Verify file exists on disk
        let path = dir.path().join("uploaded.txt");
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello upload");

        // Verify we can download it back
        let data = storages[0].download(handle).await.unwrap();
        assert_eq!(data.as_slice(), b"hello upload");
    }

    #[tokio::test]
    async fn upload_to_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Music")).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // List root to get the Music folder handle
        let items = storages[0].list_objects(None).await.unwrap();
        let music = items.iter().find(|i| i.filename == "Music").unwrap();
        assert!(music.is_folder());

        // Upload a file into Music
        let info = crate::mtp::NewObjectInfo::file("song.mp3", 5);
        storages[0]
            .upload(Some(music.handle), info, bytes_stream(b"audio"))
            .await
            .unwrap();

        assert!(dir.path().join("Music/song.mp3").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Music/song.mp3")).unwrap(),
            "audio"
        );
    }

    #[tokio::test]
    async fn delete_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("doomed.txt"), "goodbye").unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let items = storages[0].list_objects(None).await.unwrap();
        let obj = &items[0];

        storages[0].delete(obj.handle).await.unwrap();
        assert!(!dir.path().join("doomed.txt").exists());
    }

    #[tokio::test]
    async fn create_folder() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        storages[0].create_folder(None, "NewFolder").await.unwrap();

        assert!(dir.path().join("NewFolder").is_dir());
    }

    #[tokio::test]
    async fn rename_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old_name.txt"), "content").unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let items = storages[0].list_objects(None).await.unwrap();
        let obj = &items[0];

        storages[0]
            .rename(obj.handle, "new_name.txt")
            .await
            .unwrap();

        assert!(!dir.path().join("old_name.txt").exists());
        assert!(dir.path().join("new_name.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new_name.txt")).unwrap(),
            "content"
        );
    }

    #[tokio::test]
    async fn nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("a/b/c/deep.txt"), "deep").unwrap();
        std::fs::write(dir.path().join("a/top.txt"), "top").unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // List root
        let root_items = storages[0].list_objects(None).await.unwrap();
        assert_eq!(root_items.len(), 1); // Only "a"
        assert_eq!(root_items[0].filename, "a");
        assert!(root_items[0].is_folder());
    }

    #[tokio::test]
    async fn read_only_storage_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "data").unwrap();

        let config = test_config_readonly(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // Verify read-only access capability is reported
        assert_ne!(
            storages[0].info().access_capability,
            crate::ptp::AccessCapability::ReadWrite
        );

        // Upload should fail
        let info = crate::mtp::NewObjectInfo::file("new.txt", 4);
        let result = storages[0].upload(None, info, bytes_stream(b"data")).await;
        assert!(result.is_err());

        // Delete should fail
        let items = storages[0].list_objects(None).await.unwrap();
        let result = storages[0].delete(items[0].handle).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn multiple_storages() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir1.path().join("file1.txt"), "storage1").unwrap();
        std::fs::write(dir2.path().join("file2.txt"), "storage2").unwrap();

        let config = test_config_multi(&[dir1.path(), dir2.path()]);
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        assert_eq!(storages.len(), 2);

        let items1 = storages[0].list_objects(None).await.unwrap();
        assert_eq!(items1.len(), 1);
        assert_eq!(items1[0].filename, "file1.txt");

        let items2 = storages[1].list_objects(None).await.unwrap();
        assert_eq!(items2.len(), 1);
        assert_eq!(items2[0].filename, "file2.txt");
    }

    #[tokio::test]
    async fn free_space_reflects_content() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let free_before = storages[0].info().free_space_bytes;

        // Upload a file
        let info = crate::mtp::NewObjectInfo::file("big.bin", 1000);
        let data = vec![0u8; 1000];
        storages[0]
            .upload(None, info, bytes_stream(&data))
            .await
            .unwrap();

        // Re-fetch storage info
        let storages2 = device.storages().await.unwrap();
        let free_after = storages2[0].info().free_space_bytes;

        assert!(free_after < free_before);
        assert_eq!(free_before - free_after, 1000);
    }

    #[tokio::test]
    async fn events_generated_on_upload() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let info = crate::mtp::NewObjectInfo::file("event_test.txt", 5);
        storages[0]
            .upload(None, info, bytes_stream(b"hello"))
            .await
            .unwrap();

        // Events should be available (ObjectAdded + StorageInfoChanged)
        use tokio::time::{timeout, Duration};
        let event = timeout(Duration::from_millis(100), device.next_event()).await;
        assert!(event.is_ok());
    }

    #[tokio::test]
    async fn events_generated_on_delete() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("to_delete.txt"), "bye").unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let items = storages[0].list_objects(None).await.unwrap();
        storages[0].delete(items[0].handle).await.unwrap();

        // Should have ObjectRemoved + StorageInfoChanged events
        use tokio::time::{timeout, Duration};
        let event = timeout(Duration::from_millis(100), device.next_event()).await;
        assert!(event.is_ok());
    }

    #[tokio::test]
    async fn no_events_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();

        // No operations performed, so no events
        let result = device.next_event().await;
        assert!(matches!(result, Err(crate::Error::Timeout)));
    }

    #[tokio::test]
    async fn copy_object() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("original.txt"), "copy me").unwrap();
        std::fs::create_dir(dir.path().join("dest")).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let items = storages[0].list_objects(None).await.unwrap();
        let original = items.iter().find(|i| i.filename == "original.txt").unwrap();
        let dest = items.iter().find(|i| i.filename == "dest").unwrap();

        storages[0]
            .copy_object(original.handle, dest.handle, None)
            .await
            .unwrap();

        // Both should exist
        assert!(dir.path().join("original.txt").exists());
        assert!(dir.path().join("dest/original.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dest/original.txt")).unwrap(),
            "copy me"
        );
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path());

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // Try to upload a file with ".." in the name
        let info = crate::mtp::NewObjectInfo::file("../escape.txt", 6);
        let result = storages[0]
            .upload(None, info, bytes_stream(b"escape"))
            .await;
        assert!(result.is_err(), "path traversal upload should be rejected");

        // Verify the file was NOT created outside the backing dir
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    }

    #[tokio::test]
    async fn move_object() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("moveme.txt"), "move me").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        let items = storages[0].list_objects(None).await.unwrap();
        let moveme = items.iter().find(|i| i.filename == "moveme.txt").unwrap();
        let target = items.iter().find(|i| i.filename == "target").unwrap();

        storages[0]
            .move_object(moveme.handle, target.handle, None)
            .await
            .unwrap();

        assert!(!dir.path().join("moveme.txt").exists());
        assert!(dir.path().join("target/moveme.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("target/moveme.txt")).unwrap(),
            "move me"
        );

        // Should emit ObjectInfoChanged + StorageInfoChanged events
        use tokio::time::{timeout, Duration};
        let event = timeout(Duration::from_millis(100), device.next_event())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            crate::mtp::DeviceEvent::ObjectInfoChanged { .. }
        ));
        let event = timeout(Duration::from_millis(100), device.next_event())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            crate::mtp::DeviceEvent::StorageInfoChanged { .. }
        ));
    }

    /// Helper: poll for an event, retrying on Timeout up to the deadline.
    async fn poll_event_with_retry(
        device: &MtpDevice,
        timeout_duration: std::time::Duration,
    ) -> Option<crate::mtp::DeviceEvent> {
        let deadline = tokio::time::Instant::now() + timeout_duration;
        loop {
            match device.next_event().await {
                Ok(event) => return Some(event),
                Err(crate::Error::Timeout) => {
                    if tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    }

    #[tokio::test]
    async fn fs_watcher_detects_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize the backing dir to avoid macOS /var vs /private/var mismatches
        let backing_dir = dir.path().canonicalize().unwrap();
        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: "test-fswatch".into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: backing_dir.clone(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::from_millis(50),
            watch_backing_dirs: true,
        };

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();

        // Write a file directly to the backing dir (bypassing MTP)
        std::fs::write(backing_dir.join("external.txt"), "hello from outside").unwrap();

        // Poll for events — the watcher should detect the file creation.
        let event = poll_event_with_retry(&device, Duration::from_secs(5)).await;
        assert!(
            event.is_some(),
            "expected event from fs watcher, got nothing"
        );
        let event = event.unwrap();
        assert!(
            matches!(event, crate::mtp::DeviceEvent::ObjectAdded { .. }),
            "expected ObjectAdded, got {:?}",
            event
        );
    }

    /// Helper: create a virtual device with a pre-existing subdirectory and
    /// return (device, backing_dir, tempdir-guard).
    async fn virtual_device_with_subdirectory(
        serial: &str,
    ) -> (MtpDevice, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let backing_dir = dir.path().canonicalize().unwrap();
        std::fs::create_dir(backing_dir.join("Music")).unwrap();

        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: serial.into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: backing_dir.clone(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::from_millis(50),
            watch_backing_dirs: true,
        };

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();

        // Drain any startup events (macOS FSEvents may report the watched dir).
        while poll_event_with_retry(&device, Duration::from_millis(500))
            .await
            .is_some()
        {}

        (device, backing_dir, dir)
    }

    #[tokio::test]
    async fn fs_watcher_detects_file_creation_in_subdirectory() {
        let (device, backing_dir, _dir) =
            virtual_device_with_subdirectory("test-fswatch-subdir-create").await;

        std::fs::write(backing_dir.join("Music/song.mp3"), "fake mp3 data").unwrap();

        let event = poll_event_with_retry(&device, Duration::from_secs(5)).await;
        assert!(
            event.is_some(),
            "expected event from fs watcher for file in subdirectory, got nothing"
        );
        assert!(
            matches!(event.unwrap(), crate::mtp::DeviceEvent::ObjectAdded { .. }),
            "expected ObjectAdded"
        );
    }

    #[tokio::test]
    async fn fs_watcher_detects_file_rename_in_subdirectory() {
        let (device, backing_dir, _dir) =
            virtual_device_with_subdirectory("test-fswatch-subdir-rename").await;

        // Create a file and drain its events.
        std::fs::write(backing_dir.join("Music/song.mp3"), "fake mp3 data").unwrap();
        while poll_event_with_retry(&device, Duration::from_secs(5))
            .await
            .is_some()
        {}

        // Rename the file within the subdirectory.
        std::fs::rename(
            backing_dir.join("Music/song.mp3"),
            backing_dir.join("Music/track.mp3"),
        )
        .unwrap();

        // A rename should produce an ObjectRemoved (old name) and ObjectAdded (new name).
        let mut events = Vec::new();
        while let Some(event) = poll_event_with_retry(&device, Duration::from_secs(5)).await {
            events.push(event);
            let has_removed = events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. }));
            let has_added = events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectAdded { .. }));
            if has_removed && has_added {
                break;
            }
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. })),
            "expected ObjectRemoved for rename source, got {:?}",
            events
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectAdded { .. })),
            "expected ObjectAdded for rename target, got {:?}",
            events
        );
    }

    #[tokio::test]
    async fn fs_watcher_detects_file_removal_in_subdirectory() {
        let (device, backing_dir, _dir) =
            virtual_device_with_subdirectory("test-fswatch-subdir-remove").await;

        // Create a file and drain its events.
        std::fs::write(backing_dir.join("Music/song.mp3"), "fake mp3 data").unwrap();
        while poll_event_with_retry(&device, Duration::from_secs(5))
            .await
            .is_some()
        {}

        // Delete the file.
        std::fs::remove_file(backing_dir.join("Music/song.mp3")).unwrap();

        let mut events = Vec::new();
        while let Some(event) = poll_event_with_retry(&device, Duration::from_secs(5)).await {
            events.push(event);
            if events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. }))
            {
                break;
            }
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. })),
            "expected ObjectRemoved for file in subdirectory, got {:?}",
            events
        );
    }

    #[tokio::test]
    async fn fs_watcher_detects_file_removal() {
        let dir = tempfile::tempdir().unwrap();
        let backing_dir = dir.path().canonicalize().unwrap();

        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: "test-fswatch-rm".into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: backing_dir.clone(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::from_millis(50),
            watch_backing_dirs: true,
        };

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();

        // Create the file AFTER the watcher is running, so we get a clean event sequence
        std::fs::write(backing_dir.join("will_be_removed.txt"), "bye").unwrap();

        // Drain events until no more arrive (consume the ObjectAdded from creation)
        while poll_event_with_retry(&device, Duration::from_millis(500))
            .await
            .is_some()
        {}

        // Now remove the file directly (bypassing MTP)
        std::fs::remove_file(backing_dir.join("will_be_removed.txt")).unwrap();

        // Collect all events and look for ObjectRemoved
        let mut events = Vec::new();
        while let Some(event) = poll_event_with_retry(&device, Duration::from_secs(5)).await {
            events.push(event);
            // Stop after we find what we need or have collected enough
            if events.len() >= 10 {
                break;
            }
            if events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. }))
            {
                break;
            }
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. })),
            "expected ObjectRemoved among events, got {:?}",
            events
        );
    }

    #[tokio::test]
    async fn fs_watcher_dedup_suppresses_mtp_events() {
        let dir = tempfile::tempdir().unwrap();
        let backing_dir = dir.path().canonicalize().unwrap();
        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: "test-fswatch-dedup".into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: backing_dir.clone(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::from_millis(50),
            watch_backing_dirs: true,
        };

        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // Upload via MTP — should produce exactly the MTP-generated events
        let info = crate::mtp::NewObjectInfo::file("dedup_test.txt", 5);
        storages[0]
            .upload(None, info, bytes_stream(b"hello"))
            .await
            .unwrap();

        // Drain all events with a generous window for the watcher to fire.
        // MTP upload produces 1 ObjectAdded + 1 StorageInfoChanged.
        // The watcher sees the file creation but finds the handle already exists
        // in state.objects (inserted by the MTP handler under the mutex), so it
        // skips the event — no duplicate ObjectAdded.
        // We count ObjectAdded specifically because some platforms (Linux inotify)
        // may generate additional filesystem events (StorageInfoChanged etc.).
        let mut object_added_count = 0;
        let mut total_events = 0;
        while let Some(event) = poll_event_with_retry(&device, Duration::from_millis(500)).await {
            if matches!(event, crate::mtp::DeviceEvent::ObjectAdded { .. }) {
                object_added_count += 1;
            }
            total_events += 1;
            if total_events > 10 {
                break;
            }
        }

        // Exactly 1 ObjectAdded from the MTP handler. The watcher's dedup
        // suppresses duplicates.
        assert_eq!(
            object_added_count, 1,
            "expected exactly 1 ObjectAdded event, got {} (dedup may have failed)",
            object_added_count
        );
    }

    fn test_config_with_serial(dir: &std::path::Path, serial: &str) -> VirtualDeviceConfig {
        VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: serial.into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: dir.to_path_buf(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        }
    }

    #[tokio::test]
    async fn rescan_detects_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        // Create initial files before opening the device.
        std::fs::write(dir.path().join("existing.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("Photos")).unwrap();
        std::fs::write(dir.path().join("Photos/pic.jpg"), "jpeg data").unwrap();

        let config = test_config_with_serial(dir.path(), "rescan-test-001");
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // List all objects to populate the in-memory tree.
        let root_items = storages[0].list_objects(None).await.unwrap();
        assert_eq!(root_items.len(), 2); // existing.txt + Photos
        let photos = root_items.iter().find(|i| i.filename == "Photos").unwrap();
        let _sub_items = storages[0].list_objects(Some(photos.handle)).await.unwrap();

        // Drain any events from listing.
        while device.next_event().await.is_ok() {}

        // --- Externally modify the backing dir (bypassing MTP) ---
        // Delete existing.txt
        std::fs::remove_file(dir.path().join("existing.txt")).unwrap();
        // Create a new file
        std::fs::write(dir.path().join("new_file.txt"), "I'm new").unwrap();
        // Delete the file inside Photos
        std::fs::remove_file(dir.path().join("Photos/pic.jpg")).unwrap();

        // Rescan via the public API.
        let summary = crate::rescan_virtual_device("rescan-test-001").unwrap();
        assert_eq!(
            summary.removed, 2,
            "should remove existing.txt and Photos/pic.jpg"
        );
        assert_eq!(summary.added, 1, "should add new_file.txt");

        // Verify the object tree now matches the filesystem.
        let root_items = storages[0].list_objects(None).await.unwrap();
        let names: Vec<&str> = root_items.iter().map(|i| i.filename.as_str()).collect();
        assert!(
            names.contains(&"new_file.txt"),
            "new_file.txt should appear: {:?}",
            names
        );
        assert!(
            names.contains(&"Photos"),
            "Photos dir still exists: {:?}",
            names
        );
        assert!(
            !names.contains(&"existing.txt"),
            "existing.txt should be gone: {:?}",
            names
        );

        // Verify Photos subdirectory is now empty.
        let photos = root_items.iter().find(|i| i.filename == "Photos").unwrap();
        let sub_items = storages[0].list_objects(Some(photos.handle)).await.unwrap();
        assert!(
            sub_items.is_empty(),
            "Photos should be empty after pic.jpg was removed"
        );

        // Verify events were queued (ObjectRemoved, ObjectAdded, StorageInfoChanged).
        let mut event_types = Vec::new();
        loop {
            match device.next_event().await {
                Ok(e) => event_types.push(e),
                Err(crate::Error::Timeout) => break,
                Err(_) => break,
            }
        }
        assert!(
            event_types
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectAdded { .. })),
            "expected ObjectAdded event from rescan"
        );
        assert!(
            event_types
                .iter()
                .any(|e| matches!(e, crate::mtp::DeviceEvent::ObjectRemoved { .. })),
            "expected ObjectRemoved event from rescan"
        );
    }

    #[test]
    fn rescan_nonexistent_serial_returns_none() {
        assert!(
            crate::transport::virtual_device::registry::rescan_virtual_device("no-such-device")
                .is_none()
        );
    }

    #[tokio::test]
    async fn rescan_no_changes_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stable.txt"), "unchanged").unwrap();

        let config = test_config_with_serial(dir.path(), "rescan-test-002");
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();

        // Populate the object tree.
        let _ = storages[0].list_objects(None).await.unwrap();

        let summary = crate::rescan_virtual_device("rescan-test-002").unwrap();
        assert_eq!(summary.added, 0);
        assert_eq!(summary.removed, 0);

        drop(device);
    }

    #[tokio::test]
    async fn list_objects_resolves_full_size_for_files_larger_than_4gb() {
        // Create a 5 GB sparse file on the backing filesystem. Its ObjectInfo size
        // field will saturate at u32::MAX; the real u64 size must be resolved via
        // GetObjectPropValue(ObjectSize).
        const REAL_SIZE: u64 = 5 * 1024 * 1024 * 1024;

        let dir = tempfile::tempdir().unwrap();
        let big_path = dir.path().join("movie.mkv");
        let file = std::fs::File::create(&big_path).unwrap();
        file.set_len(REAL_SIZE).unwrap();
        drop(file);

        let config = test_config(dir.path());
        let device = MtpDevice::builder().open_virtual(config).await.unwrap();
        let storages = device.storages().await.unwrap();
        let objects = storages[0].list_objects(None).await.unwrap();

        let movie = objects
            .iter()
            .find(|o| o.filename == "movie.mkv")
            .expect("movie.mkv not found in listing");
        assert_eq!(
            movie.size, REAL_SIZE,
            "full u64 size should be resolved for files >4 GB"
        );
    }
}
