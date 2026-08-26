# Virtual device transport

Feature-gated (`virtual-device`) Transport implementation backed by local filesystem directories instead of USB. Exercises the full MTP/PTP binary protocol path.

## Architecture

```
MtpDevice (unchanged)
  → MtpDeviceInner (unchanged)
    → PtpSession (unchanged)
      → Arc<dyn Transport>
        → VirtualTransport — implements Transport trait
          → VirtualDeviceState — in-memory object tree + filesystem
```

## Module structure

- `config.rs` — `VirtualDeviceConfig`, `VirtualStorageConfig` (public types)
- `state.rs` — `VirtualDeviceState`, `VirtualObject`, `PendingCommand`, handle management
- `builders.rs` — binary payload builders (DeviceInfo, StorageInfo, ObjectInfo, containers)
- `handlers.rs` — protocol operation handlers dispatched by opcode
- `registry.rs` — global virtual device registry for discovery integration (`list_devices`, `open_by_location`, `open_by_serial`) + active-state registry for `rescan_virtual_device()` and `pause_watcher()`/`WatcherGuard`
- `watcher.rs` — filesystem watcher for detecting out-of-band changes to backing directories
- `mod.rs` — `VirtualTransport` struct + `Transport` impl + tests

## Key decisions

- **`std::sync::Mutex`** over `parking_lot::Mutex`: `parking_lot` is only a dev-dep. Virtual transport isn't performance-critical, so std mutex is fine.
- **`PendingCommand` struct**: When the host sends a command that expects a data phase (SendObjectInfo, SendObject, SetObjectPropValue), the command is stored as a `PendingCommand` in `state.pending_command`. The next `send_bulk` (data container) takes it via `.take()` and dispatches both together. This keeps pending state separate from the response queue.
- **`VecDeque` for queues**: `response_queue` and `event_queue` use `VecDeque` for O(1) front removal (FIFO access pattern).
- **Discovery via global registry**: Virtual devices can be registered via `register_virtual_device()` to appear in `MtpDevice::list_devices()`. They get synthetic location IDs starting at `0xFFFF_0000_0000_0000` to avoid collisions with real USB devices. Uses `OnceLock` for the static registry.
- **Event poll interval**: `VirtualTransport` stores `event_poll_interval: Duration` outside the mutex. When no events are pending, `receive_interrupt` awaits this delay before returning `Timeout`, preventing CPU spin in event loops. Tests use `Duration::ZERO` for speed; production callers should use 50ms+.
- **Filesystem watcher**: Controlled by `VirtualDeviceConfig::watch_backing_dirs`. When `true`, a `notify::RecommendedWatcher` watches all backing dirs recursively. When files are written/deleted directly (bypassing MTP), the watcher detects changes and queues `ObjectAdded`/`ObjectRemoved` events. Gated behind `virtual-device` feature via the `notify` dependency. Tests that don't need the watcher should set this to `false` for faster startup and no background threads.
- **Watcher scope**: The filesystem watcher only tracks file/directory creation and removal. Content modifications to existing files are intentionally ignored — they don't change the object tree and would be noisy (editors write temp files, do atomic renames, etc.). Real MTP devices are also inconsistent about emitting `ObjectInfoChanged` for content edits.
- **Dedup for watcher events**: Uses state-based dedup rather than TTL tracking. MTP handlers modify the filesystem while holding the `state` mutex and insert/remove handles before releasing the lock. The watcher callback also acquires `state` before processing events. For creates, the watcher skips events when a handle already exists for the path. For removes, the watcher skips when no handle is found (already removed by the MTP handler). No extra tracking structure or timing assumptions needed. Events for the backing directory itself (empty relative path) are skipped — macOS FSEvents reports the watched directory as "created" on startup.
- **Canonical backing dirs**: `VirtualDeviceState::new()` canonicalizes all backing dirs at startup. This ensures consistent path comparison between handlers and the watcher callback (important on macOS where `/var` → `/private/var`).
- **Rescan via active-state registry**: `VirtualTransport::new()` registers its `Arc<Mutex<VirtualDeviceState>>` in a second global registry keyed by serial number. `rescan_virtual_device(serial)` looks up the state and calls `rescan_backing_dirs()`, which diffs the in-memory object tree against the filesystem, removing stale entries and adding new ones. The transport unregisters on drop. This avoids the fs watcher's latency (200-500ms on macOS FSEvents) and handles rapid delete+recreate sequences that the watcher can miss.
- **Watcher pause/resume**: `pause_watcher(serial)` returns a `WatcherGuard` (RAII) that sets `watcher_paused = true` on the device state. While paused, the watcher callback drops all events. The guard resumes on drop (poison-safe via `lock().ok()`). This prevents a race condition where external code deletes and recreates files in the backing directory: without pausing, the OS can deliver stale deletion events after a rescan has already re-added the objects.

## Gotchas

- `list_objects(None)` applies a parent filter (`ParentFilter::Exact(ROOT)`), so the virtual transport must set `parent = ObjectHandle::ROOT` on root-level objects for them to appear.
- `SendObjectInfo` for folders creates the directory immediately (no `SendObject` phase needed for folders per MTP spec).
- Storage IDs start at `0x00010001` (matching real MTP convention).
- The global registries (device registry + active-state registry) are process-wide and shared across tests. Registry tests must clean up with `unregister_virtual_device()` and use unique serial numbers to avoid interference. Rescan tests must also use unique serials.
- `event_poll_interval` lives on `VirtualTransport` (not inside `VirtualDeviceState`) because we need it after dropping the mutex lock and before an async `.await`.
- Fs watcher tests use canonicalized backing dirs and `poll_event_with_retry` to handle macOS FSEvents latency. The removal test drains create events first because macOS may coalesce or reorder events.
- `GetPartialObject64` (0x95C1) has a **different param layout** from `GetPartialObject` (0x101B): 4 params (handle, offset_lo, offset_hi, max_bytes) vs 3 (handle, offset, max_bytes). The two handlers share `read_partial()` for the actual file I/O but parse params separately.
