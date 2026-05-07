use mtp_rs::ptp::{ObjectHandle, ObjectInfo};
use mtp_rs::{MtpDevice, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, map_mtp_error, open_mtp_session};
use crate::output::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsReport {
    pub path: String,
    pub storage_id: u32,
    pub entries: Vec<LsEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LsEntry {
    pub id: u32,
    pub parent_id: u32,
    pub storage_id: u32,
    pub kind: ObjectKind,
    pub name: String,
    pub size: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ObjectKind {
    File,
    Folder,
}

enum RemoteTarget {
    Root,
    Object(ObjectInfo),
}

pub fn run_ls(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
) -> Result<LsReport, AppError> {
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = read_ls(&session.device, remote_path).await;
        let close_result = session.close().await;

        match (result, close_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

async fn read_ls(device: &MtpDevice, remote_path: &str) -> Result<LsReport, AppError> {
    let storages = device.storages().await.map_err(map_mtp_error)?;
    let storage = storages.first().ok_or_else(|| AppError::Mtp {
        message: "TP-7 reported no MTP storages.".to_string(),
    })?;
    let path = normalize_remote_path(remote_path)?;
    let target = resolve_path(storage, &path).await?;
    let mut entries = match target {
        RemoteTarget::Root => list_entries(storage, None).await?,
        RemoteTarget::Object(object) if object.is_folder() => {
            list_entries(storage, Some(object.handle)).await?
        }
        RemoteTarget::Object(object) => vec![LsEntry::from_object(object)],
    };

    sort_entries(&mut entries);

    Ok(LsReport {
        path,
        storage_id: storage.id().0,
        entries,
    })
}

async fn resolve_path(storage: &Storage, path: &str) -> Result<RemoteTarget, AppError> {
    let components = path_components(path)?;
    if components.is_empty() {
        return Ok(RemoteTarget::Root);
    }

    let mut parent = None;

    for (index, component) in components.iter().enumerate() {
        let children = storage.list_objects(parent).await.map_err(map_mtp_error)?;
        let object = children
            .into_iter()
            .find(|object| object.filename == *component)
            .ok_or_else(|| AppError::RemotePathNotFound {
                path: format_path(&components[..=index]),
            })?;

        if index == components.len() - 1 {
            return Ok(RemoteTarget::Object(object));
        }

        if !object.is_folder() {
            return Err(AppError::RemotePathNotDirectory {
                path: format_path(&components[..=index]),
            });
        }

        parent = Some(object.handle);
    }

    Ok(RemoteTarget::Root)
}

async fn list_entries(
    storage: &Storage,
    parent: Option<ObjectHandle>,
) -> Result<Vec<LsEntry>, AppError> {
    let objects = storage.list_objects(parent).await.map_err(map_mtp_error)?;

    Ok(objects.into_iter().map(LsEntry::from_object).collect())
}

fn normalize_remote_path(path: &str) -> Result<String, AppError> {
    let components = path_components(path)?;
    Ok(format_path(&components))
}

fn path_components(path: &str) -> Result<Vec<String>, AppError> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return Ok(Vec::new());
    }

    let components = trimmed
        .trim_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .map(|component| {
            if component == "." || component == ".." {
                return Err(AppError::InvalidRemotePath {
                    path: path.to_string(),
                    message: "remote paths cannot contain . or .. components".to_string(),
                });
            }

            Ok(component.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(components)
}

fn format_path(components: &[String]) -> String {
    if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    }
}

fn sort_entries(entries: &mut [LsEntry]) {
    entries.sort_by(|left, right| {
        let left_kind = kind_sort_key(&left.kind);
        let right_kind = kind_sort_key(&right.kind);

        left_kind
            .cmp(&right_kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn kind_sort_key(kind: &ObjectKind) -> u8 {
    match kind {
        ObjectKind::Folder => 0,
        ObjectKind::File => 1,
    }
}

impl LsEntry {
    fn from_object(object: ObjectInfo) -> Self {
        Self {
            id: object.handle.0,
            parent_id: object.parent.0,
            storage_id: object.storage_id.0,
            kind: if object.is_folder() {
                ObjectKind::Folder
            } else {
                ObjectKind::File
            },
            name: object.filename,
            size: object.size,
            modified: object.modified.and_then(|modified| modified.format()),
        }
    }
}

#[cfg(test)]
mod tests {
    use mtp_rs::ptp::{DateTime, ObjectFormatCode, ObjectHandle, ObjectInfo, StorageId};

    use super::*;

    #[test]
    fn normalizes_empty_and_root_paths_to_root() {
        assert_eq!(normalize_remote_path("").unwrap(), "/");
        assert_eq!(normalize_remote_path("/").unwrap(), "/");
    }

    #[test]
    fn normalizes_repeated_separators() {
        assert_eq!(
            normalize_remote_path("//Recordings//Take 1//").unwrap(),
            "/Recordings/Take 1"
        );
    }

    #[test]
    fn rejects_parent_path_components() {
        let error = normalize_remote_path("/Recordings/../System").unwrap_err();

        assert!(matches!(error, AppError::InvalidRemotePath { .. }));
    }

    #[test]
    fn converts_object_info_to_listing_entry() {
        let object = ObjectInfo {
            handle: ObjectHandle(42),
            storage_id: StorageId(65_537),
            parent: ObjectHandle(7),
            filename: "take.wav".to_string(),
            size: 1024,
            modified: Some(DateTime::new(2026, 5, 7, 12, 30, 45).unwrap()),
            ..Default::default()
        };

        let entry = LsEntry::from_object(object);

        assert_eq!(entry.id, 42);
        assert_eq!(entry.parent_id, 7);
        assert_eq!(entry.storage_id, 65_537);
        assert_eq!(entry.kind, ObjectKind::File);
        assert_eq!(entry.name, "take.wav");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.modified.as_deref(), Some("20260507T123045"));
    }

    #[test]
    fn sorts_folders_before_files_by_name() {
        let mut entries = vec![
            entry("z.wav", ObjectKind::File),
            entry("Recordings", ObjectKind::Folder),
            entry("a.wav", ObjectKind::File),
        ];

        sort_entries(&mut entries);

        assert_eq!(
            entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            vec!["Recordings", "a.wav", "z.wav"]
        );
    }

    #[test]
    fn parses_root_listing_fixture() {
        let fixture = include_str!("../tests/fixtures/tp7-root-listing.json");
        let report = serde_json::from_str::<LsReport>(fixture).unwrap();

        assert_eq!(report.path, "/");
        assert_eq!(report.storage_id, 65_537);
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].kind, ObjectKind::Folder);
    }

    fn entry(name: &str, kind: ObjectKind) -> LsEntry {
        let format = match kind {
            ObjectKind::File => ObjectFormatCode::Wav,
            ObjectKind::Folder => ObjectFormatCode::Association,
        };
        LsEntry::from_object(ObjectInfo {
            filename: name.to_string(),
            format,
            ..Default::default()
        })
    }
}
