//! Error types for mtp-rs.

use thiserror::Error;

/// The main error type for mtp-rs operations.
#[derive(Debug, Error)]
pub enum Error {
    /// USB communication error
    #[error("USB error: {0}")]
    Usb(#[from] nusb::Error),

    /// Protocol-level error from device
    #[error("Protocol error: {code:?} during {operation:?}")]
    Protocol {
        /// The response code returned by the device.
        code: crate::ptp::ResponseCode,
        /// The operation that triggered the error.
        operation: crate::ptp::OperationCode,
    },

    /// Invalid data received from device
    #[error("Invalid data: {message}")]
    InvalidData {
        /// Description of what was invalid.
        message: String,
    },

    /// I/O error
    #[error("I/O error: {0}")]
    Io(std::io::Error),

    /// Operation timed out
    #[error("Operation timed out")]
    Timeout,

    /// Device was disconnected
    #[error("Device disconnected")]
    Disconnected,

    /// Session not open
    #[error("Session not open")]
    SessionNotOpen,

    /// No device found
    #[error("No MTP device found")]
    NoDevice,

    /// Operation cancelled
    #[error("Operation cancelled")]
    Cancelled,
}

impl Error {
    /// Create an invalid data error with a message.
    #[must_use]
    pub fn invalid_data(message: impl Into<String>) -> Self {
        Error::InvalidData {
            message: message.into(),
        }
    }

    /// Check if this is a retryable error.
    ///
    /// Retryable errors are transient and the operation may succeed if retried:
    /// - `DeviceBusy`: Device is temporarily busy
    /// - `Timeout`: Operation timed out but device may still be responsive
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Protocol {
                code: crate::ptp::ResponseCode::DeviceBusy,
                ..
            } | Error::Timeout
        )
    }

    /// Get the response code if this is a protocol error.
    #[must_use]
    pub fn response_code(&self) -> Option<crate::ptp::ResponseCode> {
        match self {
            Error::Protocol { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Check if this error indicates another process has exclusive access to the device.
    ///
    /// This typically happens on macOS when `ptpcamerad` or another application
    /// has already claimed the USB interface. Applications can use this to provide
    /// platform-specific guidance to users.
    ///
    /// # Example
    ///
    /// ```ignore
    /// match device.open().await {
    ///     Err(e) if e.is_exclusive_access() => {
    ///         // On macOS, likely ptpcamerad interference
    ///         // App can query IORegistry for UsbExclusiveOwner to get details
    ///         show_exclusive_access_help();
    ///     }
    ///     Err(e) => handle_other_error(e),
    ///     Ok(dev) => use_device(dev),
    /// }
    /// ```
    #[must_use]
    pub fn is_exclusive_access(&self) -> bool {
        match self {
            Error::Usb(io_err) => {
                let msg = io_err.to_string().to_lowercase();
                // macOS: "could not be opened for exclusive access"
                // Linux: typically EBUSY, but message varies
                // Windows: "access denied" or similar
                msg.contains("exclusive access")
                    || msg.contains("device or resource busy")
                    || (msg.contains("access") && msg.contains("denied"))
            }
            Error::Io(io_err) => {
                let msg = io_err.to_string().to_lowercase();
                msg.contains("exclusive access")
                    || msg.contains("device or resource busy")
                    || (msg.contains("access") && msg.contains("denied"))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error as IoError, ErrorKind};

    #[test]
    fn test_is_exclusive_access_macos_message() {
        // macOS nusb error message (tested via Io variant; same logic as Usb variant)
        let io_err = IoError::other("could not be opened for exclusive access");
        let err = Error::Io(io_err);
        assert!(err.is_exclusive_access());
    }

    #[test]
    fn test_is_exclusive_access_linux_busy() {
        // Linux EBUSY style message (tested via Io variant; same logic as Usb variant)
        let io_err = IoError::other("Device or resource busy");
        let err = Error::Io(io_err);
        assert!(err.is_exclusive_access());
    }

    #[test]
    fn test_is_exclusive_access_windows_denied() {
        // Windows access denied style message (tested via Io variant; same logic as Usb variant)
        let io_err = IoError::new(ErrorKind::PermissionDenied, "Access is denied");
        let err = Error::Io(io_err);
        assert!(err.is_exclusive_access());
    }

    #[test]
    fn test_is_exclusive_access_io_error() {
        // Also works for Io variant
        let io_err = IoError::other("could not be opened for exclusive access");
        let err = Error::Io(io_err);
        assert!(err.is_exclusive_access());
    }

    #[test]
    fn test_is_exclusive_access_false_for_other_errors() {
        assert!(!Error::Timeout.is_exclusive_access());
        assert!(!Error::Disconnected.is_exclusive_access());
        assert!(!Error::NoDevice.is_exclusive_access());
        assert!(!Error::invalid_data("some error").is_exclusive_access());

        let io_err = IoError::new(ErrorKind::NotFound, "device not found");
        assert!(!Error::Io(io_err).is_exclusive_access());
    }
}
