use crate::error::AppError;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use std::time::Duration;

/// Largest frame `write_frame` sends and `read_frame` accepts.
pub const MAX_FRAME_SIZE: usize = 10_000_000;

/// Time allowed to send or receive one frame.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Bytes requested from the stream per read call. A receive buffer grows by at most
/// this amount before the corresponding data has arrived.
const READ_CHUNK: usize = 64 * 1024;

/// Write a length-prefixed frame: [4-byte big-endian len][payload].
/// A zero-length frame signals end of stream.
///
/// Returns an error if `data` is longer than `MAX_FRAME_SIZE` or if the write does not
/// complete within `FRAME_TIMEOUT`.
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), AppError> {
    if data.len() > MAX_FRAME_SIZE {
        return Err(AppError::Other(format!(
            "frame too large to send: {} bytes (max {MAX_FRAME_SIZE})",
            data.len()
        )));
    }
    let len = u32::try_from(data.len())
        .map_err(|_| AppError::Other("frame length does not fit in 32 bits".into()))?;
    tokio::time::timeout(FRAME_TIMEOUT, async {
        send.write_all(&len.to_be_bytes()).await?;
        if !data.is_empty() {
            send.write_all(data).await?;
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|_| AppError::Other("frame write timed out".into()))?
}

/// Read a length-prefixed frame of at most `MAX_FRAME_SIZE` bytes within `FRAME_TIMEOUT`.
/// Returns None on zero-length (end of stream).
pub async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>, AppError> {
    read_frame_limited(recv, MAX_FRAME_SIZE).await
}

/// Read a length-prefixed frame of at most `max_len` bytes within `FRAME_TIMEOUT`.
/// Returns None on zero-length (end of stream).
///
/// The buffer grows in steps of `READ_CHUNK` as data arrives, so a declared length
/// alone does not allocate memory.
pub async fn read_frame_limited(
    recv: &mut RecvStream,
    max_len: usize,
) -> Result<Option<Vec<u8>>, AppError> {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 {
            return Ok(None);
        }
        if len > max_len {
            return Err(AppError::Other(format!(
                "frame too large: {len} bytes (max {max_len})"
            )));
        }
        let mut buf = Vec::new();
        while buf.len() < len {
            let start = buf.len();
            let end = len.min(start + READ_CHUNK);
            buf.resize(end, 0);
            let Some(tail) = buf.get_mut(start..end) else {
                return Err(AppError::Other("frame buffer range out of bounds".into()));
            };
            recv.read_exact(tail).await?;
        }
        Ok(Some(buf))
    })
    .await
    .map_err(|_| AppError::Other("frame read timed out".into()))?
}

/// Read `recv` until the remote finishes the stream, within `timeout`.
/// Returns an error if more than `limit` bytes arrive.
pub async fn read_to_end_timeout(
    recv: &mut RecvStream,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, AppError> {
    let bytes = tokio::time::timeout(timeout, recv.read_to_end(limit))
        .await
        .map_err(|_| AppError::Other("stream read timed out".into()))??;
    Ok(bytes)
}

/// Write `data` to `send` and finish the stream, within `timeout`.
pub async fn write_all_timeout(
    send: &mut SendStream,
    data: &[u8],
    timeout: Duration,
) -> Result<(), AppError> {
    tokio::time::timeout(timeout, send.write_all(data))
        .await
        .map_err(|_| AppError::Other("stream write timed out".into()))??;
    send.finish()?;
    Ok(())
}

/// Wait until the remote closes `conn`, for at most `timeout`.
///
/// A handler calls this after its response so the connection is not dropped before the
/// remote has read the response.
pub async fn wait_closed(conn: &Connection, timeout: Duration) {
    let _ = tokio::time::timeout(timeout, conn.closed()).await;
}
