use crate::error::AppError;

const MAX_FRAME_SIZE: usize = 10_000_000;

/// Write a length-prefixed frame: [4-byte big-endian len][payload].
/// A zero-length frame signals end of stream.
pub async fn write_frame(
    send: &mut iroh::endpoint::SendStream,
    data: &[u8],
) -> Result<(), AppError> {
    let len = data.len() as u32;
    send.write_all(&len.to_be_bytes()).await?;
    if !data.is_empty() {
        send.write_all(data).await?;
    }
    Ok(())
}

/// Read a length-prefixed frame. Returns None on zero-length (end of stream).
pub async fn read_frame(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<Option<Vec<u8>>, AppError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME_SIZE {
        return Err(AppError::Other(format!("frame too large: {len} bytes")));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(Some(buf))
}
