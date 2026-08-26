//! Global registry of virtual devices for discovery integration.
//!
//! Virtual devices registered here appear in `MtpDevice::list_devices()` and
//! can be opened via `open_by_location()` or `open_by_serial()`.

use super::config::VirtualDeviceConfig;
use super::state::{RescanSummary, VirtualDeviceState};
use crate::mtp::MtpDeviceInfo;
use std::sync::{Arc, Mutex, OnceLock};

/// Base for synthetic location IDs (high range, won't collide with real USB).
const VIRTUAL_LOCATION_BASE: u64 = 0xFFFF_0000_0000_0000;

/// A registered virtual device.
struct VirtualRegistration {
    info: MtpDeviceInfo,
    config: VirtualDeviceConfig,
}

/// Holds registered devices and a monotonically increasing index for unique location IDs.
struct Registry {
    devices: Vec<VirtualRegistration>,
    next_index: u64,
}

/// Access the global registry singleton.
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            devices: Vec::new(),
            next_index: 0,
        })
    })
}

/// Register a virtual device so it appears in `MtpDevice::list_devices()`.
///
/// Returns the `MtpDeviceInfo` with a synthetic location ID. Use this location
/// ID with `MtpDevice::open_by_location()` or the serial with
/// `MtpDevice::open_by_serial()` to open the device.
#[must_use]
pub fn register_virtual_device(config: &VirtualDeviceConfig) -> MtpDeviceInfo {
    let mut reg = registry().lock().unwrap();
    let index = reg.next_index;
    reg.next_index += 1;
    let location_id = VIRTUAL_LOCATION_BASE + index;

    let info = MtpDeviceInfo {
        vendor_id: 0xFFFF,
        product_id: 0x0001,
        manufacturer: Some(config.manufacturer.clone()),
        product: Some(config.model.clone()),
        serial_number: Some(config.serial.clone()),
        location_id,
    };

    reg.devices.push(VirtualRegistration {
        info: info.clone(),
        config: config.clone(),
    });

    info
}

/// Remove a registered virtual device by location ID.
pub fn unregister_virtual_device(location_id: u64) {
    let mut reg = registry().lock().unwrap();
    reg.devices.retain(|r| r.info.location_id != location_id);
}

/// Get all registered virtual devices (called by `list_devices`).
pub(crate) fn list_virtual_devices() -> Vec<MtpDeviceInfo> {
    let reg = registry().lock().unwrap();
    reg.devices.iter().map(|r| r.info.clone()).collect()
}

/// Try to find a virtual device config by location ID.
pub(crate) fn find_virtual_config_by_location(location_id: u64) -> Option<VirtualDeviceConfig> {
    let reg = registry().lock().unwrap();
    reg.devices
        .iter()
        .find(|r| r.info.location_id == location_id)
        .map(|r| r.config.clone())
}

/// Try to find a virtual device config by serial number.
pub(crate) fn find_virtual_config_by_serial(serial: &str) -> Option<VirtualDeviceConfig> {
    let reg = registry().lock().unwrap();
    reg.devices
        .iter()
        .find(|r| r.info.serial_number.as_deref() == Some(serial))
        .map(|r| r.config.clone())
}

// --- Active device state registry ---
//
// When a VirtualTransport is created, it registers its shared state here so
// that `rescan_virtual_device()` can look it up by serial number. Entries are
// removed when the transport is dropped.

/// An entry in the active-states registry: (serial, shared state).
type ActiveEntry = (String, Arc<Mutex<VirtualDeviceState>>);

/// Access the global active-states registry.
fn active_states() -> &'static Mutex<Vec<ActiveEntry>> {
    static ACTIVE: OnceLock<Mutex<Vec<ActiveEntry>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register an active virtual device's state (called by `VirtualTransport::new`).
pub(super) fn register_active_state(serial: String, state: Arc<Mutex<VirtualDeviceState>>) {
    let mut active = active_states().lock().unwrap();
    active.push((serial, state));
}

/// Unregister an active virtual device's state (called when `VirtualTransport` is dropped).
pub(super) fn unregister_active_state(serial: &str) {
    let mut active = active_states().lock().unwrap();
    if let Some(pos) = active.iter().position(|(s, _)| s == serial) {
        active.remove(pos);
    }
}

/// RAII guard that resumes the filesystem watcher when dropped.
///
/// Created by [`pause_watcher`]. While this guard is alive, all filesystem
/// events for the device are silently dropped.
pub struct WatcherGuard {
    state: Arc<Mutex<VirtualDeviceState>>,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.watcher_paused = false;
        }
    }
}

/// Pause the filesystem watcher for a virtual device, returning a guard that
/// resumes it on drop.
///
/// While paused, filesystem events are silently dropped. This prevents stale
/// OS-level events from corrupting the object tree when files in the backing
/// directory are deleted and recreated externally (outside of MTP). Without
/// pausing, delayed deletion events can arrive after a rescan has already
/// re-added the objects, incorrectly removing them.
///
/// Returns `None` if no active virtual device with that serial exists.
///
/// # Example
///
/// ```rust,no_run
/// use mtp_rs::{pause_watcher, rescan_virtual_device};
///
/// {
///     let _guard = pause_watcher("my-device-serial").unwrap();
///
///     // ... delete and recreate files in the backing directory ...
///
///     rescan_virtual_device("my-device-serial");
/// } // watcher resumes here when `_guard` is dropped
/// ```
pub fn pause_watcher(serial: &str) -> Option<WatcherGuard> {
    let active = active_states().lock().unwrap();
    let state_arc = active
        .iter()
        .find(|(s, _)| s == serial)
        .map(|(_, state)| Arc::clone(state))?;
    drop(active);
    state_arc.lock().unwrap().watcher_paused = true;
    Some(WatcherGuard { state: state_arc })
}

/// Force a rescan of a virtual device's backing directories, identified by
/// serial number.
///
/// This diffs the in-memory object tree against the actual filesystem and
/// queues `ObjectAdded`/`ObjectRemoved`/`StorageInfoChanged` events for any
/// differences found.
///
/// Returns `Some(summary)` with the number of added/removed objects, or
/// `None` if no active virtual device with that serial exists.
///
/// # When to use
///
/// Call this after manipulating files in the backing directory directly on
/// disk (outside of MTP) when you need the virtual device to reflect those
/// changes immediately. This avoids waiting for the filesystem watcher's
/// latency (200–500ms on macOS). For delete+recreate sequences, pair with
/// [`pause_watcher`] to prevent stale events from corrupting state.
///
/// # Example
///
/// ```rust,no_run
/// use mtp_rs::rescan_virtual_device;
///
/// // After manipulating files in the backing directory...
/// if let Some(summary) = rescan_virtual_device("my-device-serial") {
///     println!("Rescan: {} added, {} removed", summary.added, summary.removed);
/// }
/// ```
pub fn rescan_virtual_device(serial: &str) -> Option<RescanSummary> {
    let active = active_states().lock().unwrap();
    let state_arc = active
        .iter()
        .find(|(s, _)| s == serial)
        .map(|(_, state)| Arc::clone(state))?;
    drop(active); // Release the registry lock before acquiring the state lock.
    let mut state = state_arc.lock().unwrap();
    Some(state.rescan_backing_dirs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::virtual_device::config::VirtualStorageConfig;
    use std::time::Duration;

    fn make_config(serial: &str) -> (VirtualDeviceConfig, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Virtual Phone".into(),
            serial: serial.into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: dir.path().to_path_buf(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        };
        (config, dir)
    }

    #[test]
    fn register_and_list() {
        let (config, _dir) = make_config("reg-test-001");
        let info = register_virtual_device(&config);

        assert!(info.location_id >= VIRTUAL_LOCATION_BASE);
        assert_eq!(info.serial_number.as_deref(), Some("reg-test-001"));
        assert_eq!(info.manufacturer.as_deref(), Some("TestCorp"));

        let devices = list_virtual_devices();
        assert!(devices
            .iter()
            .any(|d| d.serial_number.as_deref() == Some("reg-test-001")));

        // Clean up
        unregister_virtual_device(info.location_id);
    }

    #[test]
    fn find_by_location() {
        let (config, _dir) = make_config("reg-test-002");
        let info = register_virtual_device(&config);

        let found = find_virtual_config_by_location(info.location_id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().serial, "reg-test-002");

        // Clean up
        unregister_virtual_device(info.location_id);
    }

    #[test]
    fn find_by_serial() {
        let (config, _dir) = make_config("reg-test-003");
        let info = register_virtual_device(&config);

        let found = find_virtual_config_by_serial("reg-test-003");
        assert!(found.is_some());
        assert_eq!(found.unwrap().serial, "reg-test-003");

        // Not found
        assert!(find_virtual_config_by_serial("nonexistent").is_none());

        // Clean up
        unregister_virtual_device(info.location_id);
    }

    #[test]
    fn unregister() {
        let (config, _dir) = make_config("reg-test-004");
        let info = register_virtual_device(&config);

        unregister_virtual_device(info.location_id);

        assert!(find_virtual_config_by_location(info.location_id).is_none());
        assert!(find_virtual_config_by_serial("reg-test-004").is_none());
    }

    #[test]
    fn location_id_unique_after_unregister() {
        let (config_a, _dir_a) = make_config("reg-test-unique-a");
        let info_a = register_virtual_device(&config_a);

        let (config_b, _dir_b) = make_config("reg-test-unique-b");
        let info_b = register_virtual_device(&config_b);

        // Unregister A
        unregister_virtual_device(info_a.location_id);

        // Register C — must get a unique location_id different from both A and B
        let (config_c, _dir_c) = make_config("reg-test-unique-c");
        let info_c = register_virtual_device(&config_c);

        assert_ne!(info_c.location_id, info_a.location_id);
        assert_ne!(info_c.location_id, info_b.location_id);

        // Clean up
        unregister_virtual_device(info_b.location_id);
        unregister_virtual_device(info_c.location_id);
    }

    #[tokio::test]
    async fn open_by_location_integration() {
        let dir = tempfile::tempdir().unwrap();
        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Registry Phone".into(),
            serial: "reg-test-005".into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: dir.path().to_path_buf(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        };
        let info = register_virtual_device(&config);

        let device = crate::MtpDevice::builder()
            .open_by_location(info.location_id)
            .await
            .unwrap();
        assert_eq!(device.device_info().model, "Registry Phone");

        // Clean up
        unregister_virtual_device(info.location_id);
    }

    #[tokio::test]
    async fn open_by_serial_integration() {
        let dir = tempfile::tempdir().unwrap();
        let config = VirtualDeviceConfig {
            manufacturer: "TestCorp".into(),
            model: "Registry Phone".into(),
            serial: "reg-test-006".into(),
            storages: vec![VirtualStorageConfig {
                description: "Internal Storage".into(),
                capacity: 1024 * 1024 * 1024,
                backing_dir: dir.path().to_path_buf(),
                read_only: false,
            }],
            supports_rename: true,
            event_poll_interval: Duration::ZERO,
            watch_backing_dirs: false,
        };
        let info = register_virtual_device(&config);

        let device = crate::MtpDevice::builder()
            .open_by_serial("reg-test-006")
            .await
            .unwrap();
        assert_eq!(device.device_info().model, "Registry Phone");

        // Clean up
        unregister_virtual_device(info.location_id);
    }
}
