//! USB transport abstraction layer.

#[cfg(test)]
pub mod mock;
pub mod nusb;

#[cfg(feature = "virtual-device")]
pub mod virtual_device;

pub use self::nusb::{NusbTransport, UsbDeviceInfo};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::time::Duration;

/// A boxed stream of byte chunks used by [`Transport::send_bulk_streaming`].
pub type BulkStream<'a> = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send + 'a>>;

/// Transport trait for MTP/PTP communication.
///
/// Abstracts USB communication to enable testing with mock transport.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send data on the bulk OUT endpoint.
    async fn send_bulk(&self, data: &[u8]) -> Result<(), crate::Error>;

    /// Send data as a continuous bulk transfer from a stream of chunks.
    ///
    /// All chunks are sent as one logical USB transfer, properly terminated
    /// with a short packet or ZLP. This avoids buffering the entire payload
    /// in memory.
    ///
    /// The default implementation collects all chunks and calls `send_bulk`.
    /// USB transports should override this to use native streaming writes.
    async fn send_bulk_streaming(&self, chunks: BulkStream<'_>) -> Result<(), crate::Error> {
        use futures::StreamExt;
        let mut buffer = Vec::new();
        let mut stream = chunks;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(crate::Error::Io)?;
            buffer.extend_from_slice(&chunk);
        }
        self.send_bulk(&buffer).await
    }

    /// Receive data from the bulk IN endpoint.
    ///
    /// `max_size` is the maximum bytes to receive in one call.
    async fn receive_bulk(&self, max_size: usize) -> Result<Vec<u8>, crate::Error>;

    /// Receive event data from the interrupt IN endpoint.
    ///
    /// This may block until an event is available.
    async fn receive_interrupt(&self) -> Result<Vec<u8>, crate::Error>;

    /// Cancel an in-progress transfer using the USB Still Image Class mechanism.
    ///
    /// Sends a CLASS_CANCEL control request (`bRequest=0x64`), then drains
    /// remaining data from the bulk IN and interrupt pipes. After this call,
    /// the session is clean and ready for the next operation.
    ///
    /// `idle_timeout` controls how long to wait during drain steps before
    /// assuming each pipe is clear. 300ms (matching libmtp / Windows) is
    /// the recommended default; see [`mtp::DEFAULT_CANCEL_TIMEOUT`](crate::mtp::DEFAULT_CANCEL_TIMEOUT).
    async fn cancel_transfer(
        &self,
        transaction_id: u32,
        idle_timeout: Duration,
    ) -> Result<(), crate::Error>;
}
