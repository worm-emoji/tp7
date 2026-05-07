use serde::Serialize;

use crate::device::{Tp7Device, UsbMode, list_tp7_devices, select_one_device};
use crate::output::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectReport {
    pub serial_number: Option<String>,
    pub mode: UsbMode,
    pub mtp_ready: bool,
    pub message: String,
}

pub fn run_connect(serial: Option<&str>) -> Result<ConnectReport, AppError> {
    let device = select_one_device(list_tp7_devices()?, serial)?;

    match device.mode {
        UsbMode::Mtp | UsbMode::Mixed => Ok(ready_report(&device)),
        _ => Err(AppError::MtpNotVisible {
            serial: device
                .serial_number
                .clone()
                .unwrap_or_else(|| "<no-serial>".to_string()),
            mode: device.mode.to_string(),
        }),
    }
}

fn ready_report(device: &Tp7Device) -> ConnectReport {
    ConnectReport {
        serial_number: device.serial_number.clone(),
        mode: device.mode.clone(),
        mtp_ready: true,
        message: "TP-7 exposes an MTP-compatible interface.".to_string(),
    }
}
