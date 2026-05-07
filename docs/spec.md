# TP-7 CLI Research Spec

## Purpose

Build a macOS CLI for Teenage Engineering TP-7 file access.

The first implementation target is a robust Rust-based MTP file manager that can detect the TP-7, switch or validate MTP mode, list files, and transfer recordings. Finder-style mounting is valuable, but should be treated as a later layer because MTP is object-based and macOS mounting requires FUSE infrastructure.

## Current Decision

Use Rust.

Recommended initial stack:

- `mtp-rs` for a pure Rust MTP implementation.
- `nusb` indirectly through `mtp-rs` for USB access.
- `clap` for CLI parsing.
- `tracing` or `log` for diagnostics.
- Later: `fuser` plus macFUSE or Fuse-T for filesystem mounting.

Avoid `libmtp` in the first prototype unless `mtp-rs` cannot handle the TP-7 reliably. FieldKit uses `libmtp`, so it remains a proven fallback.

## Connected Device Findings

Observed on macOS with the TP-7 plugged in and powered on.

Device identity:

- Product: `TP-7`
- Vendor string: `teenage engineering`
- Vendor ID: `0x2367`
- Product ID: `0x0019`
- Serial: `F1RTL11C`
- USB speed: high speed, 480 Mbps
- USB version: 2.0
- Current USB configuration: `1`
- Number of USB configurations: `3`

The current mode is not MTP. macOS sees the TP-7 as a composite USB audio/MIDI device:

- Interface 0: USB Audio Control
- Interface 1: USB Audio Streaming input
- Interface 2: USB Audio Streaming output
- Interface 3: USB MIDI

Current owners:

- `usbaudiod` owns the audio interfaces.
- `MIDIServer` owns the MIDI interface and also appears as the device-level exclusive owner.
- `FieldKit.app` has a USB device user client attached.
- `Dia.app` also had a USB device user client attached during inspection.

A descriptor-only libusb probe showed all three available USB configurations expose the same audio/MIDI interface set in the current device mode. No MTP or mass-storage interface was visible at that point.

Important inference: TP-7 probably needs a device-specific mode switch before it re-enumerates as MTP. The MTP interface is not merely hidden in another visible configuration while the device is in audio/MIDI mode.

## FieldKit Findings

The installed `/Applications/FieldKit.app` is strong evidence for how Teenage Engineering handles this on macOS.

Observed from local binary metadata:

- FieldKit version: `2.0`, bundle build `210`.
- It links against bundled `libmtp.dylib`.
- It links against bundled `libusb.dylib`.
- It has the `com.apple.security.device.usb` entitlement.
- It does not appear to bundle macFUSE/Fuse-T components.

Observed strings and logs indicate:

- FieldKit has a `MTPService`.
- FieldKit has USB probing code.
- FieldKit detects mass storage, MIDI, and MTP interfaces.
- FieldKit knows whether a device `supportsMTP` and `supportsStartingMTP`.
- FieldKit can re-enumerate the device.
- FieldKit sends MIDI `greet` and `mode` requests.
- FieldKit then opens the MTP device with libmtp.

Practical inference:

FieldKit likely sends a Teenage Engineering-specific MIDI request to switch the TP-7 from audio/MIDI mode into MTP mode, waits for USB re-enumeration, and then opens the re-enumerated MTP device through libmtp/libusb.

The exact MIDI mode-switch payload is still unknown.

## Protocol Findings

MTP is not a block storage protocol. It exposes object operations over a request/response protocol. A CLI can list, transfer, delete, and rename objects directly. A POSIX-looking mounted directory requires translation and caching.

Useful MTP concepts for implementation:

- Device sessions are explicit.
- Storage IDs identify storage roots.
- Object handles identify files/folders.
- Paths are not native protocol identifiers; the CLI must resolve paths to object handles.
- File writes are object uploads, not arbitrary block writes.
- Many POSIX write patterns do not map cleanly to MTP.
- Deletes and renames are protocol operations if supported by the device.

Core MTP operations likely needed for v1:

- Open session
- Get device info
- Get storage IDs
- Get storage info
- Get object handles
- Get object info
- Get object
- Send object info
- Send object
- Delete object
- Set object property or rename operation, depending on device support

## macOS Constraints

macOS has no native Finder-level MTP support. Teenage Engineering documents FieldKit as the macOS solution because Windows and Linux support MTP natively.

Potential process conflicts:

- FieldKit
- Dia
- Android File Transfer or similar MTP apps
- `ptpcamerad`, if a device re-enumerates as a Still Image/PTP/MTP class device

The CLI should detect and report likely conflicts rather than failing with a generic USB error.

For mounting, macOS requires a FUSE-compatible runtime:

- macFUSE: mature, but requires kernel extension approval or newer FSKit paths depending on version/macOS.
- Fuse-T: kext-less, attractive for distribution, but needs validation with Rust FUSE tooling.

V1 should not require FUSE.

## Rust Tooling

Preferred:

- `mtp-rs`: pure Rust MTP implementation, built on `nusb`. This avoids C dependencies and should be the first prototype path.
- `fuser`: Rust FUSE library for a future mount layer.

Fallback:

- `libmtp` through FFI or a small C/Rust bridge. This is proven by FieldKit but introduces a C dependency and LGPL considerations.

Open validation tasks:

- Confirm `mtp-rs` can open the TP-7 once it is in MTP mode.
- Confirm object listing and transfers work with TP-7 recordings.
- Confirm whether `mtp-rs` exposes all required operations or whether we need small extensions.
- Confirm whether `nusb` can handle any macOS device ownership edge cases cleanly.

## Go Tooling Considered

Go is feasible but less attractive for this project.

Options:

- `go-mtpfs`: useful as a historical reference for MTP/FUSE behavior.
- `go-fuse`: possible for a future mount, but macOS support is less straightforward.
- Go plus `libmtp` via cgo: likely robust but less appealing than pure Rust.

Decision: keep Go as reference material only.

## CLI Design

Executable name: `tp7`

Global flags:

- `--device <serial>`: select a specific TP-7.
- `--json`: machine-readable output.
- `--verbose`: show USB/MTP diagnostic logs.
- `--auto-connect`: allow file commands to switch into MTP mode automatically.
- `--no-progress`: disable progress bars for scripts.

### V1 Commands

`tp7 devices`

List connected TP-7 devices. Show serial, USB mode, whether MTP appears available, and whether another app appears to own the device.

`tp7 status`

Show model, serial, firmware if available, battery if available, storage capacity/free space, and current mode.

`tp7 connect`

Switch the TP-7 into MTP mode if needed, wait for re-enumeration, and validate the MTP session. In v1, file commands should not silently switch modes unless `--auto-connect` is set.

`tp7 doctor`

Diagnose common macOS problems: TP-7 not found, FieldKit/Dia/Android File Transfer conflict, MTP mode unavailable, USB permission issues, and later FUSE availability.

`tp7 ls [remote-path]`

List files/folders. Default path is `/`.

Useful flags:

- `--long`
- `--ids`
- `--json`

`tp7 tree [remote-path]`

Show recursive explorer output.

Useful flags:

- `--depth <n>`
- `--ids`
- `--json`

`tp7 stat <remote-path>`

Show metadata for one object: name, type, size, modified date, object ID, parent ID, and storage ID.

`tp7 pull <remote-path> [local-path]`

Download a file or directory from the TP-7.

Useful flags:

- `--recursive`
- `--overwrite`
- `--skip-existing`
- `--dry-run`

`tp7 push <local-path> <remote-path>`

Upload a file or directory to the TP-7.

Useful flags:

- `--recursive`
- `--overwrite`
- `--dry-run`

`tp7 mkdir <remote-path>`

Create a folder.

Useful flag:

- `--parents`

`tp7 rm <remote-path>`

Delete a file or directory.

Useful flags:

- `--recursive`
- `--force`
- `--dry-run`

`tp7 rename <remote-path> <new-name>`

Rename an object without moving it.

`tp7 eject`

Close the MTP session cleanly. If the reverse mode-switch command is discovered later, this can optionally return the device to audio/MIDI mode.

### Future Commands

`tp7 mount <mountpoint>`

Mount the TP-7 as a read-only filesystem. Requires macFUSE or Fuse-T.

`tp7 mount <mountpoint> --read-write`

Read-write mount with local write staging and upload-on-close semantics.

`tp7 sync <remote-path> <local-path>`

One-way sync from TP-7 to Mac. This is likely the most useful workflow command for recordings.

`tp7 import [local-dir]`

Opinionated copy-new-recordings command with duplicate detection.

`tp7 mv <remote-source> <remote-dest>`

Remote move if TP-7 supports it reliably. Otherwise this must be copy/delete and should wait.

`tp7 cp <remote-source> <remote-dest>`

Remote copy if TP-7 supports it reliably.

`tp7 cat <remote-file>`

Stream a small remote file to stdout.

## Interface Decisions To Confirm

Before coding, confirm these choices:

- Keep `tp7 connect` explicit by default.
- Make `--auto-connect` opt-in for commands like `ls` and `pull`.
- Use POSIX-like remote paths beginning at `/`.
- Treat multiple storage roots as hidden behind `/` unless the device exposes more than one usable storage.
- Make destructive operations require explicit paths and support `--dry-run`.
- Keep v1 mount-free.

## Implementation Plan

### Phase 0: Baseline Project

Create a small Rust CLI skeleton with command parsing, structured errors, and logging. No MTP behavior yet.

Deliverable:

- `tp7 --help`
- command stubs
- basic JSON output mode

### Phase 1: USB Detection

Detect connected TP-7 devices by vendor/product ID and serial.

Deliverable:

- `tp7 devices`
- `tp7 doctor` can identify basic conflicts
- tests around output formatting where practical

### Phase 2: MTP Mode Discovery

Learn and implement the TP-7 mode-switch path.

Candidate approaches:

- First try manual FieldKit-assisted MTP mode, then see if our tool can open MTP.
- Inspect macOS logs around FieldKit mode switching.
- Capture CoreMIDI/USB MIDI SysEx traffic if needed.
- Reproduce the MIDI mode request once understood.

Deliverable:

- `tp7 connect` can put the TP-7 into MTP mode or clearly explain what manual step is needed.

### Phase 3: MTP Session

Open an MTP session with `mtp-rs` and read basic device/storage information.

Deliverable:

- `tp7 status`
- storage capacity/free-space reporting

### Phase 4: Read-Only File Explorer

Implement object tree traversal and remote path resolution.

Deliverable:

- `tp7 ls`
- `tp7 tree`
- `tp7 stat`

### Phase 5: Transfers

Implement downloads first, then uploads.

Deliverable:

- `tp7 pull`
- `tp7 push`
- progress reporting
- skip/overwrite behavior

### Phase 6: Mutations

Implement safer write operations after listing and transfer are proven.

Deliverable:

- `tp7 mkdir`
- `tp7 rename`
- `tp7 rm`
- dry-run behavior

### Phase 7: Mount Research

Prototype a read-only FUSE mount after the direct MTP CLI is stable.

Deliverable:

- decision between macFUSE and Fuse-T
- read-only `tp7 mount <mountpoint>` prototype

## Risks

Unknown TP-7 mode-switch request:

- FieldKit strongly suggests the mode switch is MIDI-based, but the payload is not known yet.

macOS USB ownership:

- Audio/MIDI mode is owned by system services.
- MTP mode may be claimed by other apps or macOS camera services.

MTP semantics:

- Object handles can change across sessions.
- Path resolution must be built on object metadata.
- Writes cannot behave exactly like normal filesystem writes.

Library risk:

- `mtp-rs` may need fixes or extensions for TP-7 quirks.
- `libmtp` fallback is likely reliable but adds C dependency and licensing considerations.

Mount risk:

- Finder-style mounting is a separate product surface with caching, partial writes, metadata, and macOS FUSE distribution concerns.

## References

- Teenage Engineering FieldKit guide: <https://teenage.engineering/guides/fieldkit>
- Teenage Engineering MTP guide: <https://teenage.engineering/guides/mtp>
- Teenage Engineering TP-7 product page: <https://teenage.engineering/products/tp-7>
- Teenage Engineering TP-7 downloads/release notes: <https://teenage.engineering/downloads/tp-7>
- Microsoft MTP extensions overview: <https://learn.microsoft.com/en-us/windows/win32/wpd_sdk/supporting-mtp-extensions>
- `mtp-rs` crate docs: <https://docs.rs/crate/mtp-rs/0.11.0>
- `fuser` crate docs: <https://docs.rs/fuser>
- macFUSE: <https://macfuse.github.io/>
- Fuse-T: <https://www.fuse-t.org/>
- `go-mtpfs`: <https://github.com/hanwen/go-mtpfs>
