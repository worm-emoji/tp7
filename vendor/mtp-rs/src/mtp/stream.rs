//! Streaming download/upload support.

use crate::ptp::ReceiveStream;
use crate::Error;
use bytes::Bytes;
use std::ops::ControlFlow;

/// Progress information for transfers.
#[derive(Debug, Clone)]
pub struct Progress {
    /// Bytes transferred so far.
    pub bytes_transferred: u64,
    /// Total bytes (if known).
    pub total_bytes: Option<u64>,
}

impl Progress {
    /// Progress as a percentage (0.0 to 100.0).
    #[must_use]
    pub fn percent(&self) -> f64 {
        self.fraction() * 100.0
    }

    /// Progress as a fraction (0.0 to 1.0).
    #[must_use]
    pub fn fraction(&self) -> f64 {
        self.total_bytes.map_or(1.0, |total| {
            if total == 0 {
                1.0
            } else {
                self.bytes_transferred as f64 / total as f64
            }
        })
    }
}

/// Default idle timeout for cancel drain operations.
///
/// After sending the cancel control request, this is how long we wait
/// for additional data on each pipe before assuming it's clear. Matches
/// the 300ms timeout used by libmtp, which mirrors Windows behavior.
pub const DEFAULT_CANCEL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// A file download in progress with true USB streaming.
///
/// This struct wraps the low-level `ReceiveStream` and provides convenient
/// methods for tracking progress. Data is streamed directly from USB as
/// chunks arrive, without buffering the entire file in memory.
///
/// # Important
///
/// The MTP session is locked while this download is active. You must either
/// consume the entire download or call [`cancel()`](Self::cancel) before
/// dropping it. Dropping mid-download without cancelling corrupts the USB
/// session.
///
/// # Example
///
/// ```rust,no_run
/// use mtp_rs::mtp::MtpDevice;
/// use mtp_rs::ObjectHandle;
/// use tokio::io::AsyncWriteExt;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let device = MtpDevice::open_first().await?;
/// # let storages = device.storages().await?;
/// # let storage = &storages[0];
/// # let handle = ObjectHandle(1);
/// let mut download = storage.download_stream(handle).await?;
/// println!("Downloading {} bytes...", download.size());
///
/// # let mut file = tokio::fs::File::create("output.bin").await?;
/// while let Some(chunk) = download.next_chunk().await {
///     let bytes = chunk?;
///     file.write_all(&bytes).await?;
///     println!("Progress: {:.1}%", download.progress() * 100.0);
/// }
/// # Ok(())
/// # }
/// ```
#[must_use = "dropping a FileDownload mid-transfer corrupts the USB session; \
               consume it fully or call cancel()"]
pub struct FileDownload {
    size: u64,
    bytes_received: u64,
    stream: ReceiveStream,
}

impl FileDownload {
    /// Create a new FileDownload wrapping a ReceiveStream.
    pub(crate) fn new(size: u64, stream: ReceiveStream) -> Self {
        Self {
            size,
            bytes_received: 0,
            stream,
        }
    }

    /// Total file size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Bytes received so far.
    #[must_use]
    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    /// Progress as a fraction (0.0 to 1.0).
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.size == 0 {
            1.0
        } else {
            self.bytes_received as f64 / self.size as f64
        }
    }

    /// Cancel the in-progress download.
    ///
    /// Uses the USB Still Image Class cancel mechanism to stop the transfer
    /// and drain remaining data, leaving the session clean for the next
    /// operation.
    ///
    /// The `idle_timeout` controls how long to wait during pipe drain before
    /// assuming the pipe is clear. 1–2 seconds is typically sufficient.
    ///
    /// If the download is already complete, this is a no-op.
    pub async fn cancel(&mut self, idle_timeout: std::time::Duration) -> Result<(), Error> {
        self.stream.cancel(idle_timeout).await
    }

    /// Get the next chunk of data from USB.
    ///
    /// Returns `None` when the download is complete.
    pub async fn next_chunk(&mut self) -> Option<Result<Bytes, Error>> {
        match self.stream.next_chunk().await {
            Some(Ok(bytes)) => {
                self.bytes_received += bytes.len() as u64;
                Some(Ok(bytes))
            }
            Some(Err(e)) => Some(Err(e)),
            None => None,
        }
    }

    /// Consume the download and iterate with a progress callback.
    ///
    /// Calls `on_progress` after each chunk. Return `ControlFlow::Break(())`
    /// to cancel the download.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use mtp_rs::mtp::MtpDevice;
    /// use mtp_rs::ObjectHandle;
    /// use std::ops::ControlFlow;
    ///
    /// # async fn example() -> Result<(), mtp_rs::Error> {
    /// # let device = MtpDevice::open_first().await?;
    /// # let storages = device.storages().await?;
    /// # let storage = &storages[0];
    /// # let handle = ObjectHandle(1);
    /// let download = storage.download_stream(handle).await?;
    /// let data = download.collect_with_progress(|progress| {
    ///     println!("{:.1}%", progress.percent());
    ///     ControlFlow::Continue(())
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn collect_with_progress<F>(mut self, mut on_progress: F) -> Result<Vec<u8>, Error>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        let mut data = Vec::with_capacity(self.size as usize);

        while let Some(result) = self.next_chunk().await {
            let chunk = result?;
            data.extend_from_slice(&chunk);

            let progress = Progress {
                bytes_transferred: self.bytes_received,
                total_bytes: Some(self.size),
            };

            if let ControlFlow::Break(()) = on_progress(progress) {
                self.stream.cancel(DEFAULT_CANCEL_TIMEOUT).await?;
                return Err(Error::Cancelled);
            }
        }

        Ok(data)
    }

    /// Collect all remaining data into a `Vec<u8>`.
    ///
    /// This consumes the download and buffers all data in memory.
    pub async fn collect(self) -> Result<Vec<u8>, Error> {
        self.stream.collect().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;

    #[test]
    fn progress_calculations() {
        let cases = [
            (50, Some(100), 50.0, 0.5),
            (100, Some(100), 100.0, 1.0),
            (25, Some(100), 25.0, 0.25),
            (0, Some(0), 100.0, 1.0), // Empty file
            (50, None, 100.0, 1.0),   // Unknown total defaults to complete
        ];
        for (transferred, total, expected_pct, expected_frac) in cases {
            let p = Progress {
                bytes_transferred: transferred,
                total_bytes: total,
            };
            assert_eq!(
                p.percent(),
                expected_pct,
                "percent failed for {transferred}/{total:?}"
            );
            assert_eq!(
                p.fraction(),
                expected_frac,
                "fraction failed for {transferred}/{total:?}"
            );
        }

        // Large numbers
        let large = Progress {
            bytes_transferred: u64::MAX / 2,
            total_bytes: Some(u64::MAX),
        };
        let frac = large.fraction();
        assert!(frac > 0.49 && frac < 0.51);
    }

    #[tokio::test]
    async fn test_collect_with_progress_cancel_cleans_up() {
        use crate::ptp::{
            pack_u16, pack_u32, ContainerType, ObjectHandle, OperationCode, PtpSession,
            ResponseCode,
        };
        use crate::transport::mock::MockTransport;
        use std::sync::Arc;

        // Helper to build a response container
        fn response(tx_id: u32, code: ResponseCode) -> Vec<u8> {
            let mut buf = Vec::with_capacity(12);
            buf.extend_from_slice(&pack_u32(12));
            buf.extend_from_slice(&pack_u16(ContainerType::Response.to_code()));
            buf.extend_from_slice(&pack_u16(code.into()));
            buf.extend_from_slice(&pack_u32(tx_id));
            buf
        }

        // Helper to build a data container
        fn data(tx_id: u32, code: OperationCode, payload: &[u8]) -> Vec<u8> {
            let len = 12 + payload.len();
            let mut buf = Vec::with_capacity(len);
            buf.extend_from_slice(&pack_u32(len as u32));
            buf.extend_from_slice(&pack_u16(ContainerType::Data.to_code()));
            buf.extend_from_slice(&pack_u16(code.into()));
            buf.extend_from_slice(&pack_u32(tx_id));
            buf.extend_from_slice(payload);
            buf
        }

        let mock = Arc::new(MockTransport::new());
        let transport: Arc<dyn crate::transport::Transport> = Arc::clone(&mock) as _;
        mock.queue_response(response(0, ResponseCode::Ok)); // OpenSession

        let file_data = vec![1u8; 1000];
        let file_size = file_data.len() as u64;
        mock.queue_response(data(1, OperationCode::GetObject, &file_data));

        let session = Arc::new(PtpSession::open(transport, 1).await.unwrap());
        let stream = session.get_object_stream(ObjectHandle(1)).await.unwrap();
        let download = FileDownload::new(file_size, stream);

        // Break after first chunk
        let result = download
            .collect_with_progress(|_progress| ControlFlow::Break(()))
            .await;

        assert!(matches!(result, Err(Error::Cancelled)));

        // Verify cancel_transfer was called with the correct transaction ID
        let cancel_calls = mock.get_cancel_calls();
        assert_eq!(cancel_calls, vec![1]);
    }
}
