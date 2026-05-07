use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, open_mtp_session};
use crate::output::AppError;
use crate::remote::{RemoteObject, RemoteTarget, first_storage, resolve_path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatReport {
    pub path: String,
    pub object: RemoteObject,
    pub format: String,
    pub format_code: String,
    pub created: Option<String>,
}

pub fn run_stat(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
) -> Result<StatReport, AppError> {
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = read_stat(&session.device, remote_path).await;
        let close_result = session.close().await;

        match (result, close_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

async fn read_stat(device: &mtp_rs::MtpDevice, remote_path: &str) -> Result<StatReport, AppError> {
    let storage = first_storage(device).await?;
    let resolved = resolve_path(&storage, remote_path).await?;
    let RemoteTarget::Object(object) = resolved.target else {
        return Err(AppError::RemotePathIsRoot);
    };

    let format = format!("{:?}", object.format);
    let format_code = format!("0x{:04x}", u16::from(object.format));
    let created = object.created.and_then(|created| created.format());
    let remote_object = RemoteObject::from_object(object);

    Ok(StatReport {
        path: resolved.path,
        object: remote_object,
        format,
        format_code,
        created,
    })
}

#[cfg(test)]
mod tests {
    use mtp_rs::ptp::{DateTime, ObjectFormatCode, ObjectHandle, ObjectInfo, StorageId};

    use super::*;

    #[test]
    fn builds_stat_report_from_object_info() {
        let object = ObjectInfo {
            handle: ObjectHandle(1),
            storage_id: StorageId(65_537),
            format: ObjectFormatCode::Association,
            filename: "recordings".to_string(),
            created: Some(DateTime::new(2024, 1, 26, 14, 20, 0).unwrap()),
            modified: Some(DateTime::new(2026, 5, 7, 12, 0, 0).unwrap()),
            ..Default::default()
        };
        let format = format!("{:?}", object.format);
        let format_code = format!("0x{:04x}", u16::from(object.format));
        let created = object.created.and_then(|created| created.format());
        let remote_object = RemoteObject::from_object(object);
        let report = StatReport {
            path: "/recordings".to_string(),
            object: remote_object,
            format,
            format_code,
            created,
        };

        assert_eq!(report.path, "/recordings");
        assert_eq!(report.object.name, "recordings");
        assert_eq!(report.object.id, 1);
        assert_eq!(report.format, "Association");
        assert_eq!(report.format_code, "0x3001");
        assert_eq!(report.created.as_deref(), Some("20240126T142000"));
    }
}
