use crate::error::AppError;

/// Fixed 8-byte header: [seq:u32 BE][timestamp:u32 BE]
pub const HEADER_SIZE: usize = 8;

/// Encode a header + payload into a single buffer for sending on a QUIC stream.
pub fn encode_audio_frame(seq: u32, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload.len());
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&timestamp.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Decode a received buffer into (seq, timestamp, payload).
pub fn decode_audio_frame(buf: &[u8]) -> Result<(u32, u32, &[u8]), AppError> {
    if buf.len() < HEADER_SIZE {
        return Err(AppError::Other(format!(
            "audio frame too short: {} bytes",
            buf.len()
        )));
    }
    let seq = u32::from_be_bytes(buf[0..4].try_into().unwrap());
    let timestamp = u32::from_be_bytes(buf[4..8].try_into().unwrap());
    Ok((seq, timestamp, &buf[HEADER_SIZE..]))
}

/// Write a length-prefixed audio frame to a QUIC send stream.
/// Format: [len:u16 BE][header + payload]
pub async fn write_audio_frame(
    send: &mut iroh::endpoint::SendStream,
    seq: u32,
    timestamp: u32,
    payload: &[u8],
) -> Result<(), AppError> {
    let frame = encode_audio_frame(seq, timestamp, payload);
    let len = frame.len() as u16;
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(&frame).await?;
    Ok(())
}

/// Read a length-prefixed audio frame from a QUIC receive stream.
/// Returns None on clean stream close.
pub async fn read_audio_frame(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<Option<(u32, u32, Vec<u8>)>, AppError> {
    let mut len_buf = [0u8; 2];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::ReadError(iroh::endpoint::ReadError::ClosedStream)) => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    if len < HEADER_SIZE {
        return Err(AppError::Other(format!(
            "audio frame length too short: {len}"
        )));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    let (seq, timestamp, payload) = decode_audio_frame(&buf)?;
    Ok(Some((seq, timestamp, payload.to_vec())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let payload = b"opus data here";
        let encoded = encode_audio_frame(42, 960, payload);
        let (seq, ts, decoded_payload) = decode_audio_frame(&encoded).unwrap();
        assert_eq!(seq, 42);
        assert_eq!(ts, 960);
        assert_eq!(decoded_payload, payload);
    }

    #[test]
    fn decode_too_short_fails() {
        let buf = [0u8; 4]; // less than HEADER_SIZE
        assert!(decode_audio_frame(&buf).is_err());
    }

    #[test]
    fn encode_empty_payload() {
        let encoded = encode_audio_frame(0, 0, &[]);
        assert_eq!(encoded.len(), HEADER_SIZE);
        let (seq, ts, payload) = decode_audio_frame(&encoded).unwrap();
        assert_eq!(seq, 0);
        assert_eq!(ts, 0);
        assert!(payload.is_empty());
    }
}
