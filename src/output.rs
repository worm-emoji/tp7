use serde::Serialize;
use thiserror::Error;

use crate::device::{Tp7Device, interface_summary};
use crate::doctor::{DoctorReport, ProcessConflict};

#[derive(Debug, Error)]
pub enum AppError {
    #[error("USB enumeration failed: {message}")]
    UsbEnumeration { message: String },

    #[error("process inspection failed: {message}")]
    ProcessInspection { message: String },

    #[error("no TP-7 device with serial {serial} was found")]
    DeviceNotFound { serial: String },

    #[error("JSON output failed: {source}")]
    Json { source: serde_json::Error },

    #[error("{command} is not implemented yet")]
    NotImplemented { command: String },
}

impl AppError {
    pub fn not_implemented(command: impl Into<String>) -> Self {
        Self::NotImplemented {
            command: command.into(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            AppError::NotImplemented { .. } => 2,
            AppError::UsbEnumeration { .. }
            | AppError::ProcessInspection { .. }
            | AppError::DeviceNotFound { .. }
            | AppError::Json { .. } => 1,
        }
    }
}

pub fn write_devices(devices: &[Tp7Device], json: bool) -> Result<(), AppError> {
    if json {
        write_json(devices)?;
        return Ok(());
    }

    if devices.is_empty() {
        println!("No TP-7 devices found.");
        return Ok(());
    }

    for device in devices {
        println!(
            "{}  {}  {}:{}  {}  {}",
            device.serial_number.as_deref().unwrap_or("<no-serial>"),
            device.mode,
            device.vendor_id_hex,
            device.product_id_hex,
            device.speed.as_deref().unwrap_or("unknown-speed"),
            device.product.as_deref().unwrap_or("TP-7")
        );
        println!("  manufacturer: {}", display_opt(&device.manufacturer));
        println!("  usb version: {}", device.usb_version);
        println!(
            "  device version: {}",
            device.device_version.as_deref().unwrap_or("unknown")
        );
        if let Some(location_id) = &device.location_id {
            println!("  macOS location id: {location_id}");
        }
        if let Some(bus_id) = &device.bus_id {
            println!("  bus: {bus_id}");
        }
        if let Some(address) = device.device_address {
            println!("  address: {address}");
        }
        println!("  interfaces: {}", interface_summary(&device.interfaces));
    }

    Ok(())
}

pub fn write_doctor(report: &DoctorReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!("TP-7 doctor");
    for check in &report.checks {
        println!("[{}] {}: {}", check.status, check.name, check.message);
    }

    if !report.process_conflicts.is_empty() {
        println!();
        println!("Possible process conflicts:");
        for conflict in &report.process_conflicts {
            print_process_conflict(conflict);
        }
    }

    if !report.devices.is_empty() {
        println!();
        println!("Detected devices:");
        for device in &report.devices {
            println!(
                "- {} ({})",
                device.serial_number.as_deref().unwrap_or("<no-serial>"),
                device.mode
            );
        }
    }

    Ok(())
}

fn write_json<T: Serialize + ?Sized>(value: &T) -> Result<(), AppError> {
    serde_json::to_writer_pretty(std::io::stdout(), value)
        .map_err(|source| AppError::Json { source })?;
    println!();
    Ok(())
}

fn display_opt(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("unknown")
}

fn print_process_conflict(conflict: &ProcessConflict) {
    println!(
        "- pid {}: {} ({})",
        conflict.pid, conflict.name, conflict.reason
    );
}
