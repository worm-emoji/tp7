use serde::Serialize;
use thiserror::Error;

use crate::connect::ConnectReport;
use crate::device::{Tp7Device, interface_summary};
use crate::doctor::{DoctorReport, ProcessConflict};
use crate::status::StatusReport;
use crate::usb_owner::UsbOwner;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("USB enumeration failed: {message}")]
    UsbEnumeration { message: String },

    #[error("process inspection failed: {message}")]
    ProcessInspection { message: String },

    #[error("USB ownership inspection failed: {message}")]
    UsbOwnershipInspection { message: String },

    #[error("no TP-7 device with serial {serial} was found")]
    DeviceNotFound { serial: String },

    #[error("no TP-7 devices were found")]
    NoDevices,

    #[error("found {count} TP-7 devices; rerun with --device <serial>")]
    MultipleDevices { count: usize },

    #[error("TP-7 {serial} is in {mode} mode; no MTP-compatible interface is visible")]
    MtpNotVisible { serial: String, mode: String },

    #[error("MTP operation failed: {message}")]
    Mtp { message: String },

    #[error("MTP device is busy or owned by another process: {message}")]
    MtpExclusiveAccess { message: String },

    #[error("MIDI operation failed: {message}")]
    Midi { message: String },

    #[error("MIDI response timed out: {message}")]
    MidiTimeout { message: String },

    #[error("MIDI command 0x{command:02x} was rejected with status 0x{status:02x}: {message}")]
    MidiCommandRejected {
        command: u8,
        status: u8,
        message: String,
    },

    #[error("runtime initialization failed: {message}")]
    Runtime { message: String },

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
            AppError::MtpNotVisible { .. } => 3,
            AppError::MtpExclusiveAccess { .. } => 4,
            AppError::Midi { .. }
            | AppError::MidiTimeout { .. }
            | AppError::MidiCommandRejected { .. } => 5,
            AppError::UsbEnumeration { .. }
            | AppError::ProcessInspection { .. }
            | AppError::UsbOwnershipInspection { .. }
            | AppError::DeviceNotFound { .. }
            | AppError::NoDevices
            | AppError::MultipleDevices { .. }
            | AppError::Mtp { .. }
            | AppError::Runtime { .. }
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

pub fn write_status(report: &StatusReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!(
        "{}  {}  {}",
        report.usb.serial_number.as_deref().unwrap_or("<no-serial>"),
        report.usb.mode,
        report.usb.product.as_deref().unwrap_or("TP-7")
    );
    println!(
        "  mtp: {} {} ({})",
        report.mtp.manufacturer, report.mtp.model, report.mtp.device_version
    );
    println!("  mtp serial: {}", report.mtp.serial_number);
    println!("  supports rename: {}", report.mtp.supports_rename);
    println!("  storages: {}", report.mtp.storage_count);

    for storage in &report.mtp.storages {
        println!(
            "  - {}: {} free of {} bytes ({})",
            storage.id, storage.free_space_bytes, storage.max_capacity_bytes, storage.description
        );
    }

    Ok(())
}

pub fn write_connect(report: &ConnectReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!(
        "{}  {} -> {}  {}",
        report.serial_number.as_deref().unwrap_or("<no-serial>"),
        report.initial_mode,
        report.final_mode,
        report.message
    );
    println!("  switched: {}", if report.switched { "yes" } else { "no" });
    println!("  mtp session: {}", report.mtp_session.message);
    if let Some(storage_count) = report.mtp_session.storage_count {
        println!("  storages: {storage_count}");
    }
    if let Some(midi_switch) = &report.midi_switch {
        println!(
            "  midi switch: command 0x{:02x}, payload {}",
            midi_switch.command,
            midi_switch.payload.join(" ")
        );
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

    if !report.usb_owners.is_empty() {
        println!();
        println!("macOS USB owners:");
        for owner in &report.usb_owners {
            print_usb_owner(owner);
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

fn print_usb_owner(owner: &UsbOwner) {
    let pid = owner
        .pid
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let location = match owner.interface_number {
        Some(number) => format!(
            "{} {} ({})",
            owner.scope,
            number,
            owner
                .interface_name
                .as_deref()
                .unwrap_or(owner.scope_node_name.as_str())
        ),
        None => format!("{} {}", owner.scope, owner.scope_node_name),
    };

    println!(
        "- {} pid {}: {} on {}",
        owner.kind, pid, owner.process, location
    );
}
