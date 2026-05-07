use serde::Serialize;
use std::thread;
use std::time::{Duration, Instant};

use crate::device::{Tp7Device, UsbMode, list_tp7_devices, select_one_device};
use crate::midi::{MidiSwitchReport, switch_tp7_to_mtp};
use crate::output::AppError;
use crate::status::run_status;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectReport {
    pub serial_number: Option<String>,
    pub initial_mode: UsbMode,
    pub final_mode: UsbMode,
    pub mtp_ready: bool,
    pub switched: bool,
    pub midi_switch: Option<MidiSwitchReport>,
    pub mtp_session: MtpSessionCheck,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MtpSessionCheck {
    pub status: MtpSessionStatus,
    pub storage_count: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MtpSessionStatus {
    Open,
    Busy,
    Failed,
}

pub fn run_connect(serial: Option<&str>) -> Result<ConnectReport, AppError> {
    let initial_device = select_one_device(list_tp7_devices()?, serial)?;

    if is_mtp_visible(&initial_device) {
        return Ok(ready_report(&initial_device, &initial_device, false, None));
    }

    if initial_device.mode != UsbMode::AudioMidi {
        return Err(AppError::MtpNotVisible {
            serial: serial_for_error(&initial_device),
            mode: initial_device.mode.to_string(),
        });
    }

    let midi_switch = switch_tp7_to_mtp(&initial_device)?;
    let final_device = wait_for_mtp(&initial_device, serial, Duration::from_secs(12))?;

    Ok(ready_report(
        &initial_device,
        &final_device,
        true,
        Some(midi_switch),
    ))
}

fn ready_report(
    initial_device: &Tp7Device,
    final_device: &Tp7Device,
    switched: bool,
    midi_switch: Option<MidiSwitchReport>,
) -> ConnectReport {
    let mtp_session = validate_mtp_session(final_device.serial_number.as_deref());
    let message = match mtp_session.status {
        MtpSessionStatus::Open if switched => {
            "TP-7 switched to MTP mode and the MTP session opened.".to_string()
        }
        MtpSessionStatus::Open => {
            "TP-7 already exposes MTP and the MTP session opened.".to_string()
        }
        MtpSessionStatus::Busy if switched => {
            "TP-7 switched to MTP mode, but another process owns the MTP interface.".to_string()
        }
        MtpSessionStatus::Busy => {
            "TP-7 exposes MTP, but another process owns the MTP interface.".to_string()
        }
        MtpSessionStatus::Failed if switched => {
            "TP-7 switched to MTP mode, but MTP session validation failed.".to_string()
        }
        MtpSessionStatus::Failed => {
            "TP-7 exposes MTP, but MTP session validation failed.".to_string()
        }
    };

    ConnectReport {
        serial_number: final_device.serial_number.clone(),
        initial_mode: initial_device.mode.clone(),
        final_mode: final_device.mode.clone(),
        mtp_ready: is_mtp_visible(final_device),
        switched,
        midi_switch,
        mtp_session,
        message,
    }
}

fn wait_for_mtp(
    initial_device: &Tp7Device,
    serial: Option<&str>,
    timeout: Duration,
) -> Result<Tp7Device, AppError> {
    let deadline = Instant::now() + timeout;
    let fallback_serial = initial_device.serial_number.as_deref();

    while Instant::now() < deadline {
        let devices = list_tp7_devices()?;
        let devices = match serial.or(fallback_serial) {
            Some(serial) => devices
                .into_iter()
                .filter(|device| device.serial_number.as_deref() == Some(serial))
                .collect::<Vec<_>>(),
            None => devices,
        };

        if let Some(device) = devices.into_iter().find(is_mtp_visible) {
            return Ok(device);
        }

        thread::sleep(Duration::from_millis(250));
    }

    Err(AppError::MtpNotVisible {
        serial: serial_for_error(initial_device),
        mode: initial_device.mode.to_string(),
    })
}

fn validate_mtp_session(serial: Option<&str>) -> MtpSessionCheck {
    match run_status(serial) {
        Ok(report) => MtpSessionCheck {
            status: MtpSessionStatus::Open,
            storage_count: Some(report.mtp.storage_count),
            message: "MTP session opened successfully.".to_string(),
        },
        Err(AppError::MtpExclusiveAccess { message }) => MtpSessionCheck {
            status: MtpSessionStatus::Busy,
            storage_count: None,
            message,
        },
        Err(error) => MtpSessionCheck {
            status: MtpSessionStatus::Failed,
            storage_count: None,
            message: error.to_string(),
        },
    }
}

fn is_mtp_visible(device: &Tp7Device) -> bool {
    matches!(device.mode, UsbMode::Mtp | UsbMode::Mixed)
}

fn serial_for_error(device: &Tp7Device) -> String {
    device
        .serial_number
        .clone()
        .unwrap_or_else(|| "<no-serial>".to_string())
}
