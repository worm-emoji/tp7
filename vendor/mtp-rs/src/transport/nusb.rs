//! USB transport implementation using nusb.

use super::{BulkStream, Transport};
use async_trait::async_trait;
use futures::lock::Mutex;
use futures_timer::Delay;
use nusb::descriptors::{InterfaceDescriptor, TransferType};
use nusb::transfer::{
    Buffer, Bulk, ControlOut, ControlType, Direction, In, Interrupt, Out, Recipient, TransferError,
};
use nusb::MaybeFuture;
use std::time::Duration;

/// MTP interface class code (Still Image).
const MTP_CLASS_IMAGE: u8 = 0x06;
/// MTP interface class code (Vendor-specific).
const MTP_CLASS_VENDOR: u8 = 0xFF;
/// MTP subclass code.
const MTP_SUBCLASS: u8 = 0x01;
/// MTP protocol code (PTP).
const MTP_PROTOCOL: u8 = 0x01;

// USB Still Image Class (SIC) cancel constants.
/// SIC Cancel Request bRequest code.
const SIC_CANCEL_REQUEST: u8 = 0x64;
/// CancelTransaction event code for the cancel payload.
const SIC_CANCEL_EVENT_CODE: u16 = 0x4001;

/// USB device information with topology-based location ID.
#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    /// USB vendor ID
    pub vendor_id: u16,
    /// USB product ID
    pub product_id: u16,
    /// Manufacturer name (e.g., "Google", "Samsung")
    pub manufacturer: Option<String>,
    /// Product name (e.g., "Pixel 9 Pro XL")
    pub product: Option<String>,
    /// Device serial number (if available)
    pub serial_number: Option<String>,
    /// USB location identifier derived from bus and port topology (stable per port)
    pub location_id: u64,
    /// Reference to the underlying nusb device info for opening
    nusb_info: nusb::DeviceInfo,
}

impl UsbDeviceInfo {
    /// Open the USB device.
    pub fn open(&self) -> Result<nusb::Device, nusb::Error> {
        self.nusb_info.open().wait()
    }
}

/// USB transport implementation using nusb.
pub struct NusbTransport {
    interface: nusb::Interface,
    interface_number: u8,
    bulk_in: Mutex<nusb::Endpoint<Bulk, In>>,
    bulk_out: Mutex<nusb::Endpoint<Bulk, Out>>,
    interrupt_in: Mutex<nusb::Endpoint<Interrupt, In>>,
    /// Timeout for bulk transfers (sending commands, receiving data).
    timeout: Duration,
}

impl NusbTransport {
    /// Default timeout for bulk transfers (30 seconds for large file transfers).
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Default buffer size for interrupt transfers.
    const INTERRUPT_BUFFER_SIZE: usize = 64;

    /// List all available MTP devices with location IDs.
    pub fn list_mtp_devices() -> Result<Vec<UsbDeviceInfo>, crate::Error> {
        Self::list_mtp_devices_with_known(&[])
    }

    /// List all available MTP devices, including additional devices identified
    /// by the given VID/PID pairs.
    ///
    /// Devices matching the provided VID/PID pairs are included in the results
    /// even if their USB descriptors don't match standard MTP class codes.
    pub fn list_mtp_devices_with_known(
        known: &[(u16, u16)],
    ) -> Result<Vec<UsbDeviceInfo>, crate::Error> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(crate::Error::Usb)?
            .filter(|dev| Self::is_mtp_device(dev, known))
            .map(|dev| {
                let location_id = location_id_from_topology(&dev);
                UsbDeviceInfo {
                    vendor_id: dev.vendor_id(),
                    product_id: dev.product_id(),
                    manufacturer: dev.manufacturer_string().map(String::from),
                    product: dev.product_string().map(String::from),
                    serial_number: dev.serial_number().map(String::from),
                    location_id,
                    nusb_info: dev,
                }
            })
            .collect();
        Ok(devices)
    }

    /// Check if a device info represents an MTP device.
    ///
    /// A device is considered MTP if it matches standard MTP class codes, has
    /// an interface with the MTP endpoint layout, or matches one of the
    /// caller-provided VID/PID pairs (used for devices with non-standard USB
    /// descriptors that still speak MTP).
    fn is_mtp_device(dev: &nusb::DeviceInfo, known: &[(u16, u16)]) -> bool {
        // Fast path: caller-supplied known devices that may use non-standard descriptors.
        if known
            .iter()
            .any(|&(v, p)| v == dev.vendor_id() && p == dev.product_id())
        {
            return true;
        }

        // Check device class/subclass/protocol at device level.
        if Self::is_mtp_class(dev.class(), dev.subclass(), dev.protocol()) {
            return true;
        }

        // Many devices are composite (class 0) or vendor-specific (class 0xFF)
        // with MTP on one interface. Only inspect these further.
        if dev.class() != 0 && dev.class() != MTP_CLASS_VENDOR {
            return false;
        }

        // Check interface-level class info available from DeviceInfo without opening.
        for intf in dev.interfaces() {
            if Self::is_mtp_class(intf.class(), intf.subclass(), intf.protocol()) {
                return true;
            }
        }

        // Fall back to opening the device and inspecting full configuration descriptors.
        // This also catches vendor-specific interfaces (class 0xFF) that use non-standard
        // subclass/protocol but have the MTP endpoint layout (e.g. Amazon Kindle).
        if let Ok(device) = dev.open().wait() {
            if let Ok(config) = device.active_configuration() {
                for interface in config.interfaces() {
                    if let Some(alt) = interface.alt_settings().next() {
                        if Self::is_mtp_interface(&alt) {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    /// Check if class/subclass/protocol match standard MTP identifiers.
    fn is_mtp_class(class: u8, subclass: u8, protocol: u8) -> bool {
        (class == MTP_CLASS_IMAGE || class == MTP_CLASS_VENDOR)
            && subclass == MTP_SUBCLASS
            && protocol == MTP_PROTOCOL
    }

    /// Check if an interface descriptor looks like an MTP interface.
    ///
    /// Matches standard MTP class/subclass/protocol, and also vendor-specific
    /// interfaces (class 0xFF) with non-standard subclass/protocol that have
    /// the MTP endpoint layout (bulk IN + bulk OUT + interrupt IN). Some devices
    /// like Amazon Kindle use vendor-specific descriptors while still speaking MTP.
    fn is_mtp_interface(alt: &InterfaceDescriptor) -> bool {
        if Self::is_mtp_class(alt.class(), alt.subclass(), alt.protocol()) {
            return true;
        }
        // For vendor-specific class, subclass and protocol are vendor-defined,
        // so we can't rely on them. Use endpoint layout as a heuristic instead.
        alt.class() == MTP_CLASS_VENDOR && Self::has_mtp_endpoint_layout(alt)
    }

    /// Check if an interface has the MTP endpoint layout:
    /// one bulk IN, one bulk OUT, and one interrupt IN endpoint.
    fn has_mtp_endpoint_layout(alt: &InterfaceDescriptor) -> bool {
        let mut bulk_in = false;
        let mut bulk_out = false;
        let mut interrupt_in = false;
        for ep in alt.endpoints() {
            match (ep.direction(), ep.transfer_type()) {
                (Direction::In, TransferType::Bulk) => bulk_in = true,
                (Direction::Out, TransferType::Bulk) => bulk_out = true,
                (Direction::In, TransferType::Interrupt) => interrupt_in = true,
                _ => {}
            }
        }
        bulk_in && bulk_out && interrupt_in
    }

    /// Whether a `claim_interface` failure looks like the OS hasn't published
    /// the interface yet (rather than a permanent error).
    ///
    /// On macOS, vendor-class or class-0 devices that IOKit doesn't
    /// auto-configure end up with no `IOUSBHostInterface` services published,
    /// even when the device's configuration descriptor reports otherwise.
    /// The resulting `claim_interface` error is `NotFound` — there's nothing
    /// for nusb to claim — and the fix is to issue `SetConfiguration(1)`,
    /// which makes IOKit publish the interface objects.
    #[cfg(target_os = "macos")]
    fn is_interface_unpublished(e: &nusb::Error) -> bool {
        matches!(e.kind(), nusb::ErrorKind::NotFound)
    }

    /// Open a specific device and claim the MTP interface.
    pub async fn open(device: nusb::Device) -> Result<Self, crate::Error> {
        Self::open_with_timeout(device, Self::DEFAULT_TIMEOUT).await
    }

    /// Open with custom bulk transfer timeout.
    ///
    /// The interface scan first looks for a strict MTP-class interface; if none
    /// is found, it falls back to any interface with the MTP endpoint layout
    /// (bulk IN + bulk OUT + interrupt IN). This relaxed fallback supports
    /// legacy devices that report a non-standard interface class — the caller
    /// has already hand-picked the device, so the scan can be permissive at
    /// this point.
    pub async fn open_with_timeout(
        device: nusb::Device,
        timeout: Duration,
    ) -> Result<Self, crate::Error> {
        // Find the MTP interface
        let config = device.active_configuration().map_err(|e| {
            crate::Error::invalid_data(format!("Failed to get configuration: {}", e))
        })?;

        let mut mtp_interface_number = None;
        let mut bulk_in_addr = None;
        let mut bulk_out_addr = None;
        let mut interrupt_in_addr = None;

        // Two-pass scan: prefer a strictly-matching MTP interface, but fall
        // back to any interface with the MTP endpoint layout. The caller has
        // already hand-picked the device, so a permissive fallback is safe and
        // supports legacy devices that report a non-standard interface class.
        let pick = |strict: bool| {
            for interface in config.interfaces() {
                let Some(alt_setting) = interface.alt_settings().next() else {
                    continue;
                };
                let matches = if strict {
                    Self::is_mtp_interface(&alt_setting)
                } else {
                    Self::has_mtp_endpoint_layout(&alt_setting)
                };
                if matches {
                    let mut bin = None;
                    let mut bout = None;
                    let mut iin = None;
                    for endpoint in alt_setting.endpoints() {
                        match (endpoint.direction(), endpoint.transfer_type()) {
                            (Direction::Out, TransferType::Bulk) => bout = Some(endpoint.address()),
                            (Direction::In, TransferType::Bulk) => bin = Some(endpoint.address()),
                            (Direction::In, TransferType::Interrupt) => {
                                iin = Some(endpoint.address())
                            }
                            _ => {}
                        }
                    }
                    return Some((interface.interface_number(), bin, bout, iin));
                }
            }
            None
        };

        if let Some((n, bin, bout, iin)) = pick(true).or_else(|| pick(false)) {
            mtp_interface_number = Some(n);
            bulk_in_addr = bin;
            bulk_out_addr = bout;
            interrupt_in_addr = iin;
        }

        let interface_number = mtp_interface_number
            .ok_or_else(|| crate::Error::invalid_data("No MTP interface found on device"))?;

        let bulk_in_addr =
            bulk_in_addr.ok_or_else(|| crate::Error::invalid_data("No bulk IN endpoint found"))?;
        let bulk_out_addr = bulk_out_addr
            .ok_or_else(|| crate::Error::invalid_data("No bulk OUT endpoint found"))?;
        let interrupt_in_addr = interrupt_in_addr
            .ok_or_else(|| crate::Error::invalid_data("No interrupt IN endpoint found"))?;

        // Claim the interface
        //
        // macOS: IOKit doesn't publish interface services for vendor-class /
        // class-0 devices with no matching driver. Force-set configuration 1
        // so IOKit publishes them, then retry.
        let interface = match device.claim_interface(interface_number).wait() {
            Ok(iface) => iface,
            #[cfg(target_os = "macos")]
            Err(e) if Self::is_interface_unpublished(&e) => {
                device
                    .set_configuration(1)
                    .wait()
                    .map_err(crate::Error::Usb)?;
                device
                    .claim_interface(interface_number)
                    .wait()
                    .map_err(crate::Error::Usb)?
            }
            Err(e) => return Err(crate::Error::Usb(e)),
        };

        // Open endpoints
        let bulk_in = interface
            .endpoint::<Bulk, In>(bulk_in_addr)
            .map_err(crate::Error::Usb)?;
        let bulk_out = interface
            .endpoint::<Bulk, Out>(bulk_out_addr)
            .map_err(crate::Error::Usb)?;
        let interrupt_in = interface
            .endpoint::<Interrupt, In>(interrupt_in_addr)
            .map_err(crate::Error::Usb)?;

        Ok(Self {
            interface,
            interface_number,
            bulk_in: Mutex::new(bulk_in),
            bulk_out: Mutex::new(bulk_out),
            interrupt_in: Mutex::new(interrupt_in),
            timeout,
        })
    }

    /// Get the bulk transfer timeout.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Set the bulk transfer timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Convert a nusb TransferError to crate::Error.
    fn convert_transfer_error(err: TransferError) -> crate::Error {
        match err {
            // send_bulk uses transfer_blocking, which cancels the transfer on
            // timeout and returns Cancelled. Map to Timeout so that
            // Error::is_retryable() treats it correctly.
            TransferError::Cancelled => crate::Error::Timeout,
            TransferError::Disconnected => crate::Error::Disconnected,
            TransferError::Stall
            | TransferError::Fault
            | TransferError::InvalidArgument
            | TransferError::Unknown(_) => crate::Error::Io(std::io::Error::other(err.to_string())),
        }
    }
}

#[async_trait]
impl Transport for NusbTransport {
    async fn send_bulk(&self, data: &[u8]) -> Result<(), crate::Error> {
        let mut ep = self.bulk_out.lock().await;
        let buf: Buffer = data.to_vec().into();
        let completion = ep.transfer_blocking(buf, self.timeout);
        completion.status.map_err(Self::convert_transfer_error)?;
        Ok(())
    }

    async fn send_bulk_streaming(&self, chunks: BulkStream<'_>) -> Result<(), crate::Error> {
        use futures::StreamExt;

        let mut ep = self.bulk_out.lock().await;
        let max_packet_size = ep.max_packet_size();
        let transfer_size: usize = 256 * 1024; // 256KB per USB transfer
                                               // Round up to max_packet_size boundary.
        let transfer_size = (transfer_size.div_ceil(max_packet_size)).max(1) * max_packet_size;

        let mut current_buf = ep.allocate(transfer_size);
        let mut total_sent = 0usize;
        let mut stream = chunks;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(crate::Error::Io)?;
            let mut remaining = chunk.as_ref();

            while !remaining.is_empty() {
                let space = current_buf.remaining_capacity();
                let to_copy = remaining.len().min(space);
                current_buf.extend_from_slice(&remaining[..to_copy]);
                remaining = &remaining[to_copy..];

                if current_buf.remaining_capacity() == 0 {
                    ep.submit(current_buf);
                    let completion = ep
                        .wait_next_complete(self.timeout)
                        .ok_or(crate::Error::Timeout)?;
                    completion.status.map_err(Self::convert_transfer_error)?;
                    total_sent += transfer_size;
                    current_buf = ep.allocate(transfer_size);
                }
            }
        }

        // Send remaining data.
        let final_len = current_buf.len();
        if final_len > 0 {
            ep.submit(current_buf);
            let completion = ep
                .wait_next_complete(self.timeout)
                .ok_or(crate::Error::Timeout)?;
            completion.status.map_err(Self::convert_transfer_error)?;

            // If the final transfer was a multiple of max_packet_size, send ZLP
            // so the device sees a short packet delimiter.
            if final_len % max_packet_size == 0 {
                ep.submit(Buffer::new(0));
                let completion = ep
                    .wait_next_complete(self.timeout)
                    .ok_or(crate::Error::Timeout)?;
                completion.status.map_err(Self::convert_transfer_error)?;
            }
        } else if total_sent > 0 && total_sent % max_packet_size == 0 {
            // All data fit in full transfers. Send ZLP to terminate.
            ep.submit(Buffer::new(0));
            let completion = ep
                .wait_next_complete(self.timeout)
                .ok_or(crate::Error::Timeout)?;
            completion.status.map_err(Self::convert_transfer_error)?;
        }

        Ok(())
    }

    async fn receive_bulk(&self, max_size: usize) -> Result<Vec<u8>, crate::Error> {
        let mut ep = self.bulk_in.lock().await;

        // If there's no pending transfer from a previous timed-out call,
        // submit a new one. Otherwise, the pending transfer already has our
        // data in flight and we just need to wait for it.
        if ep.pending() == 0 {
            let max_packet_size = ep.max_packet_size();
            let aligned_size = align_to_packet_size(max_size, max_packet_size);
            ep.submit(Buffer::new(aligned_size));
        }

        // Wait for the transfer to complete OR the timeout to expire.
        // next_complete() is cancel-safe: dropping its future does NOT cancel
        // the underlying USB transfer. On timeout we leave the transfer pending
        // so a subsequent call picks up the in-flight data.
        let completion = futures::future::select(
            Box::pin(ep.next_complete()),
            Box::pin(Delay::new(self.timeout)),
        )
        .await;

        match completion {
            futures::future::Either::Left((comp, _)) => {
                comp.status.map_err(Self::convert_transfer_error)?;
                Ok(comp.buffer[..comp.actual_len].to_vec())
            }
            futures::future::Either::Right((_, _)) => {
                // Don't cancel the transfer — it stays pending in the endpoint.
                // next_complete() is cancel-safe, so dropping its future is fine.
                // On retry, the next call will find pending() > 0 and pick it up.
                Err(crate::Error::Timeout)
            }
        }
    }

    async fn receive_interrupt(&self) -> Result<Vec<u8>, crate::Error> {
        let mut ep = self.interrupt_in.lock().await;

        // Submit a new transfer only if none is already pending.
        if ep.pending() == 0 {
            let max_packet_size = ep.max_packet_size();
            let aligned_size = align_to_packet_size(Self::INTERRUPT_BUFFER_SIZE, max_packet_size);
            ep.submit(Buffer::new(aligned_size));
        }

        // Await indefinitely — callers handle cancellation via async
        // cancellation (e.g. tokio::time::timeout or select!).
        let completion = ep.next_complete().await;
        completion.status.map_err(Self::convert_transfer_error)?;
        Ok(completion.buffer[..completion.actual_len].to_vec())
    }

    /// USB Still Image Class cancel implementation.
    ///
    /// Getting mid-transfer cancellation right on MTP/PTP devices is notoriously
    /// tricky. Different devices react differently to cancel signals, and getting
    /// the timing wrong can leave the device unresponsive until physical replug.
    ///
    /// This implementation follows the approach proven by libmtp's
    /// `ptp_read_cancel_func` (contributed by Florent Viard in 2017, PR #2),
    /// which was developed through extensive real-device testing after discovering
    /// that naive approaches caused Nexus 6P to fail all subsequent operations
    /// and GoPro Hero 5 to freeze completely.
    ///
    /// The key insight: after sending the CLASS_CANCEL control request, the drain
    /// must start *immediately*. The device stops sending new data quickly, but
    /// data already in the USB pipeline must be read and discarded before the
    /// pipe goes idle. Inserting any delay (like polling GET_DEVICE_STATUS, which
    /// Android doesn't support anyway) causes the drain to find an empty pipe
    /// while the device's MTP state machine remains stuck.
    ///
    /// The sequence is:
    /// 1. Send CLASS_CANCEL control request (bRequest=0x64, 300ms timeout)
    /// 2. Drain bulk IN pipe (read + discard until idle_timeout, maxpacket chunks)
    /// 3. Drain interrupt pipe (consume CancelTransaction event if present)
    ///
    /// References:
    /// - libmtp PR #2: <https://github.com/libmtp/libmtp/pull/2>
    /// - USB Still Image Capture Device Definition, Section 5
    async fn cancel_transfer(
        &self,
        transaction_id: u32,
        idle_timeout: Duration,
    ) -> Result<(), crate::Error> {
        // Step 1: Send CLASS_CANCEL control request (bRequest=0x64).
        //
        // 300ms timeout matches libmtp and Windows behavior. This is a
        // class-specific control transfer on endpoint 0, independent of
        // the bulk pipes. The 6-byte payload contains the CancelTransaction
        // event code (0x4001) and the transaction ID to cancel.
        let mut payload = [0u8; 6];
        payload[0..2].copy_from_slice(&SIC_CANCEL_EVENT_CODE.to_le_bytes());
        payload[2..6].copy_from_slice(&transaction_id.to_le_bytes());

        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: SIC_CANCEL_REQUEST,
                    value: 0,
                    index: self.interface_number as u16,
                    data: &payload,
                },
                Duration::from_millis(300),
            )
            .await
            .map_err(Self::convert_transfer_error)?;

        // Step 2: Drain bulk IN pipe.
        //
        // Read and discard data in maxpacket-sized chunks until idle_timeout
        // fires (no more data arriving). The device may still be sending data
        // that was already in the USB pipeline when CLASS_CANCEL arrived. The
        // drain also catches the Response container (Transaction_Cancelled or
        // Ok) that the device sends to end the transaction.
        //
        // IMPORTANT: This must happen immediately after the control request.
        // Any delay (like polling GET_DEVICE_STATUS) allows the device to
        // give up waiting for us to read, leaving its MTP state machine stuck.
        {
            let mut ep = self.bulk_in.lock().await;
            let max_packet_size = ep.max_packet_size();
            loop {
                if ep.pending() == 0 {
                    let aligned_size = align_to_packet_size(max_packet_size, max_packet_size);
                    ep.submit(Buffer::new(aligned_size));
                }

                let drain_result = {
                    let complete_fut = ep.next_complete();
                    let timeout_fut = Delay::new(idle_timeout);
                    futures::pin_mut!(complete_fut, timeout_fut);

                    match futures::future::select(complete_fut, timeout_fut).await {
                        futures::future::Either::Left((completion, _)) => {
                            match completion.status {
                                Ok(()) => {
                                    // Check for Response container (type code 3 at bytes [4..6]).
                                    if completion.actual_len >= 6 {
                                        let type_code = u16::from_le_bytes([
                                            completion.buffer[4],
                                            completion.buffer[5],
                                        ]);
                                        if type_code == 3 {
                                            Ok(true) // Response received, done
                                        } else {
                                            Ok(false) // Data, keep draining
                                        }
                                    } else {
                                        Ok(false) // keep draining
                                    }
                                }
                                Err(TransferError::Cancelled) => Ok(true),
                                Err(TransferError::Disconnected) => Err(crate::Error::Disconnected),
                                Err(_) => Ok(true),
                            }
                        }
                        futures::future::Either::Right((_, _)) => {
                            // Idle timeout — no more data arriving, pipe is clear.
                            Ok(true)
                        }
                    }
                };

                match drain_result {
                    Ok(true) => {
                        ep.cancel_all();
                        while ep.pending() > 0 {
                            let _ = ep.next_complete().await;
                        }
                        break;
                    }
                    Ok(false) => continue,
                    Err(e) => return Err(e),
                }
            }
        }

        // Step 3: Drain interrupt pipe.
        //
        // Consume the CancelTransaction event if the device sent one. This is
        // critical for some devices — GoPro Hero 5 stops responding entirely
        // if this event is left unread on the interrupt pipe.
        {
            let mut ep = self.interrupt_in.lock().await;
            if ep.pending() == 0 {
                let max_packet_size = ep.max_packet_size();
                let aligned_size =
                    align_to_packet_size(Self::INTERRUPT_BUFFER_SIZE, max_packet_size);
                ep.submit(Buffer::new(aligned_size));
            }

            let timed_out = {
                let complete_fut = ep.next_complete();
                let timeout_fut = Delay::new(idle_timeout);
                futures::pin_mut!(complete_fut, timeout_fut);

                matches!(
                    futures::future::select(complete_fut, timeout_fut).await,
                    futures::future::Either::Right(_)
                )
            };

            if timed_out {
                ep.cancel_all();
                while ep.pending() > 0 {
                    let _ = ep.next_complete().await;
                }
            }
        }
        Ok(())
    }
}

/// Round `size` up to the nearest multiple of `packet_size`.
///
/// nusb 0.2 requires that IN transfer buffer sizes are non-zero multiples of
/// the endpoint's maximum packet size.
fn align_to_packet_size(size: usize, packet_size: usize) -> usize {
    if packet_size == 0 {
        return size.max(1);
    }
    if size == 0 {
        return packet_size;
    }
    if size % packet_size == 0 {
        size
    } else {
        ((size / packet_size) + 1) * packet_size
    }
}

/// Derive a stable location identifier from USB topology (bus + port chain).
///
/// Uses FNV-1a to hash `bus_id` and `port_chain` into a deterministic `u64`.
/// The result is stable across calls for the same physical USB port, regardless
/// of which device is plugged in.
fn location_id_from_topology(dev: &nusb::DeviceInfo) -> u64 {
    // FNV-1a 64-bit constants
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in dev.bus_id().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    // Separator so bus_id "1" + port [2,3] differs from bus_id "12" + port [3]
    hash ^= 0xFF;
    hash = hash.wrapping_mul(FNV_PRIME);
    for byte in dev.port_chain() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires real MTP device
    fn test_list_devices() {
        let devices = NusbTransport::list_mtp_devices().unwrap();
        println!("Found {} MTP devices", devices.len());
        for dev in &devices {
            println!(
                "  {:04x}:{:04x} serial={:?} location={:08x}",
                dev.vendor_id, dev.product_id, dev.serial_number, dev.location_id,
            );
        }
    }

    #[tokio::test]
    #[ignore] // Requires real MTP device
    async fn test_open_device() {
        let devices = NusbTransport::list_mtp_devices().unwrap();
        assert!(!devices.is_empty(), "No MTP device found");

        let device = devices[0].open().unwrap();
        let transport = NusbTransport::open(device).await.unwrap();

        assert_eq!(transport.timeout(), NusbTransport::DEFAULT_TIMEOUT);
    }

    #[tokio::test]
    #[ignore] // Requires real MTP device
    async fn test_timeout_configuration() {
        let devices = NusbTransport::list_mtp_devices().unwrap();
        assert!(!devices.is_empty(), "No MTP device found");

        let device = devices[0].open().unwrap();
        let custom_timeout = Duration::from_secs(60);
        let mut transport = NusbTransport::open_with_timeout(device, custom_timeout)
            .await
            .unwrap();

        assert_eq!(transport.timeout(), custom_timeout);

        // Test setter
        let new_timeout = Duration::from_secs(10);
        transport.set_timeout(new_timeout);
        assert_eq!(transport.timeout(), new_timeout);
    }

    #[test]
    fn test_align_to_packet_size() {
        // Zero size rounds up to packet_size
        assert_eq!(align_to_packet_size(0, 512), 512);
        // Size smaller than packet rounds up
        assert_eq!(align_to_packet_size(1, 512), 512);
        // Exact multiple stays the same
        assert_eq!(align_to_packet_size(512, 512), 512);
        assert_eq!(align_to_packet_size(1024, 512), 1024);
        // Non-multiple rounds up
        assert_eq!(align_to_packet_size(513, 512), 1024);
        assert_eq!(align_to_packet_size(100, 64), 128);
        // Zero packet_size edge case
        assert_eq!(align_to_packet_size(0, 0), 1);
        assert_eq!(align_to_packet_size(100, 0), 100);
    }

    #[test]
    fn test_mtp_class_detection() {
        // Image class with MTP subclass/protocol
        assert!(NusbTransport::is_mtp_class(0x06, 0x01, 0x01));

        // Vendor class with MTP subclass/protocol
        assert!(NusbTransport::is_mtp_class(0xFF, 0x01, 0x01));

        // Wrong class
        assert!(!NusbTransport::is_mtp_class(0x08, 0x01, 0x01));

        // Wrong subclass
        assert!(!NusbTransport::is_mtp_class(0x06, 0x00, 0x01));

        // Wrong protocol
        assert!(!NusbTransport::is_mtp_class(0x06, 0x01, 0x00));

        // Vendor-specific with non-standard subclass/protocol (e.g. Kindle ff/ff/00)
        assert!(!NusbTransport::is_mtp_class(0xFF, 0xFF, 0x00));
    }
}
