use crate::error::AppError;
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};

/// Fixed header size: [seq:u32 BE][timestamp:u32 BE][tag:u8] = 9 bytes.
pub const HEADER_SIZE: usize = 9;

/// Auth tag byte: normal Opus frame (no checkpoint).
pub const TAG_NORMAL: u8 = 0x00;
/// Auth tag byte: checkpoint frame carrying hash-chain proof.
pub const TAG_CHECKPOINT: u8 = 0x01;

/// Largest payload accepted in one frame: a checkpoint prefix plus one Opus packet.
pub const MAX_PAYLOAD_SIZE: usize = 4200;

/// One received audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireFrame {
    pub seq: u32,
    pub timestamp: u32,
    pub tag: u8,
    pub payload: Vec<u8>,
}

/// Encode `[len:u16 BE][seq:u32 BE][timestamp:u32 BE][tag:u8][payload]`.
pub fn encode_audio_frame(
    seq: u32,
    timestamp: u32,
    tag: u8,
    payload: &[u8],
) -> Result<Vec<u8>, AppError> {
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(AppError::Other(format!(
            "audio payload too large: {} bytes",
            payload.len()
        )));
    }
    let len = (HEADER_SIZE + payload.len()) as u16;
    let mut buf = Vec::with_capacity(2 + HEADER_SIZE + payload.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.extend_from_slice(&timestamp.to_be_bytes());
    buf.push(tag);
    buf.extend_from_slice(payload);
    Ok(buf)
}

/// Decode the bytes that follow the length prefix into (seq, timestamp, tag, payload).
pub fn decode_audio_frame(buf: &[u8]) -> Result<(u32, u32, u8, &[u8]), AppError> {
    let too_short = || AppError::Other(format!("audio frame too short: {} bytes", buf.len()));
    let (seq, rest) = buf.split_first_chunk::<4>().ok_or_else(too_short)?;
    let (timestamp, rest) = rest.split_first_chunk::<4>().ok_or_else(too_short)?;
    let (tag, payload) = rest.split_first().ok_or_else(too_short)?;
    Ok((
        u32::from_be_bytes(*seq),
        u32::from_be_bytes(*timestamp),
        *tag,
        payload,
    ))
}

/// Write one length-prefixed audio frame to a QUIC send stream with a single write.
pub async fn write_audio_frame(
    send: &mut SendStream,
    seq: u32,
    timestamp: u32,
    tag: u8,
    payload: &[u8],
) -> Result<(), AppError> {
    let frame = encode_audio_frame(seq, timestamp, tag, payload)?;
    send.write_all(&frame).await?;
    Ok(())
}

/// Read one length-prefixed audio frame from a QUIC receive stream.
/// Returns `None` if the peer finished the stream at a frame boundary.
///
/// Not cancel-safe: dropping the future after the length prefix was read loses that
/// prefix. Use it in `select!` only with branches that end the stream.
pub async fn read_audio_frame(recv: &mut RecvStream) -> Result<Option<WireFrame>, AppError> {
    let mut len_buf = [0u8; 2];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    if !(HEADER_SIZE..=HEADER_SIZE + MAX_PAYLOAD_SIZE).contains(&len) {
        return Err(AppError::Other(format!(
            "audio frame length out of range: {len}"
        )));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    let (seq, timestamp, tag, payload) = decode_audio_frame(&buf)?;
    Ok(Some(WireFrame {
        seq,
        timestamp,
        tag,
        payload: payload.to_vec(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(encoded: &[u8]) -> &[u8] {
        let (len, rest) = encoded.split_first_chunk::<2>().unwrap();
        assert_eq!(u16::from_be_bytes(*len) as usize, rest.len());
        rest
    }

    #[test]
    fn encode_decode_round_trip() {
        let payload = b"opus data here";
        let encoded = encode_audio_frame(42, 960, TAG_NORMAL, payload).unwrap();
        let (seq, ts, tag, decoded_payload) = decode_audio_frame(body(&encoded)).unwrap();
        assert_eq!(seq, 42);
        assert_eq!(ts, 960);
        assert_eq!(tag, TAG_NORMAL);
        assert_eq!(decoded_payload, payload);
    }

    #[test]
    fn decode_too_short_fails() {
        let buf = [0u8; 4]; // less than HEADER_SIZE
        assert!(decode_audio_frame(&buf).is_err());
        assert!(decode_audio_frame(&[0u8; 8]).is_err());
    }

    #[test]
    fn encode_empty_payload() {
        let encoded = encode_audio_frame(0, 0, TAG_NORMAL, &[]).unwrap();
        assert_eq!(encoded.len(), 2 + HEADER_SIZE);
        let (seq, ts, tag, payload) = decode_audio_frame(body(&encoded)).unwrap();
        assert_eq!(seq, 0);
        assert_eq!(ts, 0);
        assert_eq!(tag, TAG_NORMAL);
        assert!(payload.is_empty());
    }

    #[test]
    fn encode_decode_checkpoint_tag() {
        let payload = b"checkpoint data";
        let encoded = encode_audio_frame(99, 1234, TAG_CHECKPOINT, payload).unwrap();
        let (seq, ts, tag, decoded_payload) = decode_audio_frame(body(&encoded)).unwrap();
        assert_eq!(seq, 99);
        assert_eq!(ts, 1234);
        assert_eq!(tag, TAG_CHECKPOINT);
        assert_eq!(decoded_payload, payload);
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        let payload = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        assert!(encode_audio_frame(0, 0, TAG_NORMAL, &payload).is_err());
    }
}
