use mtp_rs::MtpDevice;
use serde::Serialize;

use crate::device::{
    TP7_PRODUCT_ID, TP7_VENDOR_ID, Tp7Device, UsbMode, list_tp7_devices, select_one_device,
};
use crate::output::AppError;

const KNOWN_TP7: &[(u16, u16)] = &[(TP7_VENDOR_ID, TP7_PRODUCT_ID)];

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub usb: Tp7Device,
    pub mtp: MtpStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct MtpStatus {
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
    pub vendor_extension: String,
    pub supports_rename: bool,
    pub storage_count: usize,
    pub storages: Vec<StorageStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageStatus {
    pub id: u32,
    pub description: String,
    pub volume_identifier: String,
    pub max_capacity_bytes: u64,
    pub free_space_bytes: u64,
    pub free_space_objects: u32,
}

pub fn run_status(serial: Option<&str>) -> Result<StatusReport, AppError> {
    let usb = select_one_device(list_tp7_devices()?, serial)?;

    if !matches!(usb.mode, UsbMode::Mtp | UsbMode::Mixed) {
        return Err(AppError::MtpNotVisible {
            serial: usb
                .serial_number
                .clone()
                .unwrap_or_else(|| "<no-serial>".to_string()),
            mode: usb.mode.to_string(),
        });
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AppError::Runtime {
            message: error.to_string(),
        })?;

    let mtp = runtime.block_on(read_mtp_status(&usb))?;

    Ok(StatusReport { usb, mtp })
}

async fn read_mtp_status(usb: &Tp7Device) -> Result<MtpStatus, AppError> {
    let device = match usb.serial_number.as_deref() {
        Some(serial) => {
            MtpDevice::builder()
                .known_devices(KNOWN_TP7)
                .open_by_serial(serial)
                .await
        }
        None => {
            MtpDevice::builder()
                .known_devices(KNOWN_TP7)
                .open_first()
                .await
        }
    }
    .map_err(map_mtp_error)?;

    let device_info = device.device_info().clone();
    let storages = device.storages().await.map_err(map_mtp_error)?;
    let storage_count = storages.len();
    let storages = storages
        .into_iter()
        .map(|storage| {
            let info = storage.info();
            StorageStatus {
                id: storage.id().0,
                description: info.description.clone(),
                volume_identifier: info.volume_identifier.clone(),
                max_capacity_bytes: info.max_capacity,
                free_space_bytes: info.free_space_bytes,
                free_space_objects: info.free_space_objects,
            }
        })
        .collect();

    let status = MtpStatus {
        manufacturer: device_info.manufacturer,
        model: device_info.model,
        device_version: device_info.device_version,
        serial_number: device_info.serial_number,
        vendor_extension: device_info.vendor_extension_desc,
        supports_rename: device.supports_rename(),
        storage_count,
        storages,
    };

    device.close().await.map_err(map_mtp_error)?;

    Ok(status)
}

fn map_mtp_error(error: mtp_rs::Error) -> AppError {
    if error.is_exclusive_access() {
        return AppError::MtpExclusiveAccess {
            message: error.to_string(),
        };
    }

    AppError::Mtp {
        message: error.to_string(),
    }
}
