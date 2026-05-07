#[cfg(any(feature = "finder-mount", test))]
use std::io;
#[cfg(feature = "finder-mount")]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(feature = "finder-mount")]
use fuser::{Config, MountOption};
#[cfg(feature = "finder-mount")]
use mtp_mount::fs::MtpFs;
use serde::Serialize;

#[cfg(not(feature = "finder-mount"))]
use crate::device::UsbMode;
#[cfg(feature = "finder-mount")]
use crate::device::{Tp7Device, UsbMode};
#[cfg(feature = "finder-mount")]
use crate::mtp_session::{MtpOpenPolicy, open_mtp_session};
use crate::output::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct MountReport {
    pub mountpoint: String,
    pub serial_number: Option<String>,
    pub initial_mode: UsbMode,
    pub final_mode: UsbMode,
    pub read_only: bool,
    pub opened_finder: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmountReport {
    pub mountpoint: String,
    pub force: bool,
    pub unmounted: bool,
    pub message: String,
}

pub fn run_mount(
    serial: Option<&str>,
    auto_connect: bool,
    mountpoint: &str,
    open_finder: bool,
    human_status: bool,
) -> Result<MountReport, AppError> {
    run_mount_impl(serial, auto_connect, mountpoint, open_finder, human_status)
}

#[cfg(feature = "finder-mount")]
fn run_mount_impl(
    serial: Option<&str>,
    auto_connect: bool,
    mountpoint: &str,
    open_finder: bool,
    human_status: bool,
) -> Result<MountReport, AppError> {
    let mountpoint = existing_mountpoint_dir(mountpoint)?;
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| AppError::Runtime {
            message: error.to_string(),
        })?;

    let session = runtime.block_on(open_mtp_session(serial, policy))?;
    let prepared = session.prepared;
    let device = session.device;
    let mountpoint_label = path_to_string(&mountpoint);
    let serial_number = prepared.usb.serial_number.clone();
    let initial_mode = prepared.initial_usb.mode.clone();
    let final_mode = prepared.usb.mode.clone();

    let mtp_fs = MtpFs::new(device, true, runtime.handle().clone());
    let mut config = Config::default();
    config.mount_options = mount_options(&mtp_fs, &prepared.usb);

    let background = fuser::spawn_mount2(mtp_fs, &mountpoint, &config).map_err(|error| {
        AppError::Mount {
            message: format!(
                "failed to mount {mountpoint_label}: {error}. Install macFUSE or Fuse-T if no FUSE runtime is available."
            ),
        }
    })?;

    let opened_finder = if open_finder {
        open_mountpoint_in_finder(&mountpoint, human_status)
    } else {
        false
    };

    if human_status {
        println!("mounted TP-7 at {mountpoint_label} (read-only)");
        println!("unmount from Finder or run: tp7 unmount {mountpoint_label}");
        let _ = io::stdout().flush();
    }

    let join_result = background.join();
    let message = match join_result {
        Ok(()) => "Unmounted by the OS.".to_string(),
        Err(error) if is_graceful_mount_end(&error) => "Unmounted by the OS.".to_string(),
        Err(error) => {
            return Err(AppError::Mount {
                message: format!("mounted filesystem ended unexpectedly: {error}"),
            });
        }
    };

    Ok(MountReport {
        mountpoint: mountpoint_label,
        serial_number,
        initial_mode,
        final_mode,
        read_only: true,
        opened_finder,
        message,
    })
}

#[cfg(not(feature = "finder-mount"))]
fn run_mount_impl(
    _serial: Option<&str>,
    _auto_connect: bool,
    mountpoint: &str,
    _open_finder: bool,
    _human_status: bool,
) -> Result<MountReport, AppError> {
    let _ = existing_mountpoint_dir(mountpoint)?;
    Err(AppError::Mount {
        message: "this binary was built without Finder mount support; rebuild with `--features finder-mount` after installing macFUSE or Fuse-T development files".to_string(),
    })
}

pub fn run_unmount(mountpoint: &str, force: bool) -> Result<UnmountReport, AppError> {
    let mountpoint = existing_path(mountpoint)?;
    let mountpoint_label = path_to_string(&mountpoint);

    if !is_mounted_at(&mountpoint)? {
        return Ok(UnmountReport {
            mountpoint: mountpoint_label,
            force,
            unmounted: false,
            message: "Mount point is not currently mounted.".to_string(),
        });
    }

    match run_diskutil_unmount(&mountpoint, force) {
        Ok(()) => Ok(UnmountReport {
            mountpoint: mountpoint_label,
            force,
            unmounted: true,
            message: "Unmounted.".to_string(),
        }),
        Err(diskutil_error) => {
            if !is_mounted_at(&mountpoint)? {
                return Ok(UnmountReport {
                    mountpoint: mountpoint_label,
                    force,
                    unmounted: true,
                    message: "Unmounted.".to_string(),
                });
            }

            match run_umount(&mountpoint, force) {
                Ok(()) => Ok(UnmountReport {
                    mountpoint: mountpoint_label,
                    force,
                    unmounted: true,
                    message: "Unmounted.".to_string(),
                }),
                Err(umount_error) => Err(AppError::Unmount {
                    message: format!(
                        "failed to unmount {mountpoint_label} with diskutil ({diskutil_error}) or umount ({umount_error})"
                    ),
                }),
            }
        }
    }
}

#[cfg(feature = "finder-mount")]
fn mount_options(fs: &MtpFs, device: &Tp7Device) -> Vec<MountOption> {
    let mut options = fs.mount_options();
    options.retain(|option| !matches!(option, MountOption::FSName(_) | MountOption::Subtype(_)));

    let fs_name = match device.serial_number.as_deref() {
        Some(serial) => format!("tp7:{serial}"),
        None => "tp7".to_string(),
    };

    options.push(MountOption::FSName(fs_name));
    options.push(MountOption::Subtype("mtp".to_string()));
    options.push(MountOption::CUSTOM("volname=TP-7".to_string()));
    options
}

fn existing_mountpoint_dir(path: &str) -> Result<PathBuf, AppError> {
    let path = existing_path(path)?;
    if !path.is_dir() {
        return Err(AppError::FileSystem {
            path: path_to_string(&path),
            message: "mount point must be a directory".to_string(),
        });
    }

    Ok(path)
}

fn existing_path(path: &str) -> Result<PathBuf, AppError> {
    let path = absolute_path(path)?;
    if !path.exists() {
        return Err(AppError::FileSystem {
            path: path_to_string(&path),
            message: "path does not exist".to_string(),
        });
    }

    path.canonicalize().map_err(|error| AppError::FileSystem {
        path: path_to_string(&path),
        message: error.to_string(),
    })
}

fn absolute_path(path: &str) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Ok(path);
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| AppError::FileSystem {
            path: ".".to_string(),
            message: error.to_string(),
        })
}

#[cfg(feature = "finder-mount")]
fn open_mountpoint_in_finder(path: &Path, human_status: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        match Command::new("open").arg(path).status() {
            Ok(status) if status.success() => true,
            Ok(status) => {
                if human_status {
                    eprintln!(
                        "warning: could not open Finder for {}: {status}",
                        path.display()
                    );
                }
                false
            }
            Err(error) => {
                if human_status {
                    eprintln!(
                        "warning: could not open Finder for {}: {error}",
                        path.display()
                    );
                }
                false
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, human_status);
        false
    }
}

fn run_diskutil_unmount(path: &Path, force: bool) -> Result<(), String> {
    let mut command = Command::new("diskutil");
    command.arg("unmount");
    if force {
        command.arg("force");
    }
    command.arg(path);
    run_unmount_command(command)
}

fn run_umount(path: &Path, force: bool) -> Result<(), String> {
    let mut command = Command::new("umount");
    if force {
        command.arg("-f");
    }
    command.arg(path);
    run_unmount_command(command)
}

fn run_unmount_command(mut command: Command) -> Result<(), String> {
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exited with {}", output.status)
    };

    Err(detail)
}

fn is_mounted_at(path: &Path) -> Result<bool, AppError> {
    let output = Command::new("mount")
        .output()
        .map_err(|error| AppError::Unmount {
            message: format!("failed to inspect mounted filesystems: {error}"),
        })?;

    if !output.status.success() {
        return Err(AppError::Unmount {
            message: format!("mount exited with {}", output.status),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(mount_output_has_mountpoint(&stdout, path))
}

fn mount_output_has_mountpoint(output: &str, path: &Path) -> bool {
    let needle = format!(" on {} (", path.display());
    output.lines().any(|line| line.contains(&needle))
}

#[cfg(any(feature = "finder-mount", test))]
fn is_graceful_mount_end(error: &io::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("not mounted")
        || message.contains("unmounted")
        || message.contains("transport endpoint is not connected")
        || message.contains("device not configured")
        || message.contains("no such file or directory")
        || message.contains("invalid argument")
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mountpoint_in_mount_output() {
        let output = "/dev/disk3s1 on /System/Volumes/Data (apfs, local)\ntp7:F1RTL11C on /Users/toto/TP-7 (mtp, nodev, nosuid, read-only)\n";

        assert!(mount_output_has_mountpoint(
            output,
            Path::new("/Users/toto/TP-7")
        ));
    }

    #[test]
    fn ignores_prefix_mountpoint_matches() {
        let output = "tp7:F1RTL11C on /Users/toto/TP-7-backup (mtp, nodev, nosuid, read-only)\n";

        assert!(!mount_output_has_mountpoint(
            output,
            Path::new("/Users/toto/TP-7")
        ));
    }

    #[test]
    fn treats_common_unmount_errors_as_graceful() {
        let error = io::Error::other("mount point is not mounted");

        assert!(is_graceful_mount_end(&error));
    }
}
