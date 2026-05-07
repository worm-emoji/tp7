use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;
use mtp_rs::ptp::{ObjectHandle, ObjectInfo};
use mtp_rs::{NewObjectInfo, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, map_mtp_error, open_mtp_session};
use crate::output::AppError;
use crate::remote::{
    RemoteTarget, first_storage, format_path, join_remote_path, path_components, resolve_path,
};

const UPLOAD_CHUNK_SIZE: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushReport {
    pub local_path: String,
    pub remote_path: String,
    pub dry_run: bool,
    pub uploaded: usize,
    pub total_bytes: u64,
    pub files: Vec<PushFileReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushFileReport {
    pub local_path: String,
    pub remote_path: String,
    pub size: u64,
    pub status: PushStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PushStatus {
    Uploaded,
    DryRun,
}

#[derive(Debug, Clone, Copy)]
pub struct PushOptions {
    pub recursive: bool,
    pub overwrite: bool,
    pub dry_run: bool,
    pub progress: bool,
}

struct RemoteDestination {
    parent: Option<ObjectHandle>,
    path: String,
    name: String,
    existing: Option<ObjectInfo>,
}

pub fn run_push(
    serial: Option<&str>,
    auto_connect: bool,
    local_path: &str,
    remote_path: &str,
    options: PushOptions,
) -> Result<PushReport, AppError> {
    let local_path = PathBuf::from(local_path);
    validate_local_source(&local_path, options)?;
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = write_push(&session.device, &local_path, remote_path, options).await;
        let close_result = session.close().await;

        match (result, close_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

async fn write_push(
    device: &mtp_rs::MtpDevice,
    local_path: &Path,
    remote_path: &str,
    options: PushOptions,
) -> Result<PushReport, AppError> {
    let storage = first_storage(device).await?;
    let destination = resolve_file_destination(&storage, local_path, remote_path).await?;
    let size = local_metadata(local_path)?.len();
    let mut report = PushReport {
        local_path: path_to_string(local_path),
        remote_path: destination.path.clone(),
        dry_run: options.dry_run,
        uploaded: 0,
        total_bytes: 0,
        files: Vec::new(),
    };

    if let Some(existing) = &destination.existing {
        if existing.is_folder() {
            return Err(AppError::RemotePathNotDirectory {
                path: destination.path,
            });
        }

        if !options.overwrite {
            return Err(AppError::RemotePathExists {
                path: destination.path,
            });
        }
    }

    if options.dry_run {
        report.files.push(file_report(
            local_path,
            &destination.path,
            size,
            PushStatus::DryRun,
        ));
        return Ok(report);
    }

    if let Some(existing) = &destination.existing {
        storage
            .delete(existing.handle)
            .await
            .map_err(map_mtp_error)?;
    }

    upload_file(&storage, local_path, &destination, size, options).await?;

    report.uploaded = 1;
    report.total_bytes = size;
    report.files.push(file_report(
        local_path,
        &destination.path,
        size,
        PushStatus::Uploaded,
    ));
    Ok(report)
}

async fn upload_file(
    storage: &Storage,
    local_path: &Path,
    destination: &RemoteDestination,
    size: u64,
    options: PushOptions,
) -> Result<ObjectHandle, AppError> {
    let stream = FileChunkStream::open(local_path)?;
    let info = NewObjectInfo::file(destination.name.clone(), size);
    let result = storage
        .upload_with_progress(destination.parent, info, stream, |progress| {
            write_progress(
                options.progress,
                progress.bytes_transferred,
                progress.total_bytes.unwrap_or(size),
            );
            ControlFlow::Continue(())
        })
        .await
        .map_err(map_mtp_error);

    finish_progress(options.progress);
    result
}

async fn resolve_file_destination(
    storage: &Storage,
    local_path: &Path,
    remote_path: &str,
) -> Result<RemoteDestination, AppError> {
    let local_name = local_file_name(local_path)?;
    let remote_path = remote_path.trim();

    if remote_path.is_empty() || remote_path == "/" {
        return Ok(RemoteDestination {
            parent: None,
            path: join_remote_path("/", &local_name),
            name: local_name,
            existing: None,
        });
    }

    if remote_path.ends_with('/') {
        let resolved = resolve_path(storage, remote_path).await?;
        return match resolved.target {
            RemoteTarget::Root => Ok(RemoteDestination {
                parent: None,
                path: join_remote_path("/", &local_name),
                name: local_name,
                existing: None,
            }),
            RemoteTarget::Object(folder) if folder.is_folder() => Ok(RemoteDestination {
                parent: Some(folder.handle),
                path: join_remote_path(&resolved.path, &local_name),
                name: local_name,
                existing: None,
            }),
            RemoteTarget::Object(_) => Err(AppError::RemotePathNotDirectory {
                path: resolved.path,
            }),
        };
    }

    match resolve_path(storage, remote_path).await {
        Ok(resolved) => match resolved.target {
            RemoteTarget::Root => Ok(RemoteDestination {
                parent: None,
                path: join_remote_path("/", &local_name),
                name: local_name,
                existing: None,
            }),
            RemoteTarget::Object(folder) if folder.is_folder() => Ok(RemoteDestination {
                parent: Some(folder.handle),
                path: join_remote_path(&resolved.path, &local_name),
                name: local_name,
                existing: None,
            }),
            RemoteTarget::Object(object) => Ok(RemoteDestination {
                parent: parent_handle(object.parent),
                path: resolved.path,
                name: object.filename.clone(),
                existing: Some(object),
            }),
        },
        Err(AppError::RemotePathNotFound { .. }) => {
            let components = path_components(remote_path)?;
            let Some(name) = components.last().cloned() else {
                return Err(AppError::RemotePathIsRoot);
            };
            let parent_components = &components[..components.len() - 1];
            let parent_path = format_path(parent_components);
            let parent = resolve_path(storage, &parent_path).await?;
            match parent.target {
                RemoteTarget::Root => Ok(RemoteDestination {
                    parent: None,
                    path: join_remote_path("/", &name),
                    name,
                    existing: None,
                }),
                RemoteTarget::Object(parent) if parent.is_folder() => Ok(RemoteDestination {
                    parent: Some(parent.handle),
                    path: join_remote_path(&parent_path, &name),
                    name,
                    existing: None,
                }),
                RemoteTarget::Object(_) => {
                    Err(AppError::RemotePathNotDirectory { path: parent_path })
                }
            }
        }
        Err(error) => Err(error),
    }
}

fn validate_local_source(local_path: &Path, options: PushOptions) -> Result<(), AppError> {
    let metadata = local_metadata(local_path)?;

    if metadata.is_dir() {
        let _ = options.recursive;
        return Err(AppError::LocalPathIsFolder {
            path: path_to_string(local_path),
        });
    }

    Ok(())
}

fn local_metadata(path: &Path) -> Result<std::fs::Metadata, AppError> {
    std::fs::metadata(path).map_err(|error| AppError::FileSystem {
        path: path_to_string(path),
        message: error.to_string(),
    })
}

fn local_file_name(path: &Path) -> Result<String, AppError> {
    let Some(file_name) = path.file_name() else {
        return Err(AppError::InvalidArguments {
            message: "local path must include a file name".to_string(),
        });
    };

    Ok(file_name.to_string_lossy().to_string())
}

fn parent_handle(parent: ObjectHandle) -> Option<ObjectHandle> {
    if parent.0 == 0 { None } else { Some(parent) }
}

fn file_report(
    local_path: &Path,
    remote_path: &str,
    size: u64,
    status: PushStatus,
) -> PushFileReport {
    PushFileReport {
        local_path: path_to_string(local_path),
        remote_path: remote_path.to_string(),
        size,
        status,
    }
}

fn write_progress(enabled: bool, transferred: u64, total: u64) {
    if !enabled {
        return;
    }

    let percent = if total == 0 {
        100.0
    } else {
        transferred as f64 / total as f64 * 100.0
    };

    eprint!("\ruploaded {transferred}/{total} bytes ({percent:.1}%)");
    let _ = io::stderr().flush();
}

fn finish_progress(enabled: bool) {
    if enabled {
        eprintln!();
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

struct FileChunkStream {
    file: File,
    chunk_size: usize,
}

impl FileChunkStream {
    fn open(path: &Path) -> Result<Self, AppError> {
        let file = File::open(path).map_err(|error| AppError::FileSystem {
            path: path_to_string(path),
            message: error.to_string(),
        })?;

        Ok(Self {
            file,
            chunk_size: UPLOAD_CHUNK_SIZE,
        })
    }
}

impl Stream for FileChunkStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut buffer = vec![0; self.chunk_size];
        match self.file.read(&mut buffer) {
            Ok(0) => Poll::Ready(None),
            Ok(bytes_read) => {
                buffer.truncate(bytes_read);
                Poll::Ready(Some(Ok(Bytes::from(buffer))))
            }
            Err(error) => Poll::Ready(Some(Err(error))),
        }
    }
}

#[cfg(test)]
mod tests {
    use mtp_rs::ptp::ObjectHandle;

    use super::*;

    #[test]
    fn maps_root_parent_to_none() {
        assert_eq!(parent_handle(ObjectHandle(0)), None);
        assert_eq!(parent_handle(ObjectHandle(7)), Some(ObjectHandle(7)));
    }

    #[test]
    fn gets_local_file_name() {
        assert_eq!(
            local_file_name(Path::new("target/test.txt")).unwrap(),
            "test.txt"
        );
    }
}
