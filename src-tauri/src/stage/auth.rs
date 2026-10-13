use crate::audio::{TAG_CHECKPOINT, TAG_NORMAL};
use crate::error::AppError;
use iroh::{PublicKey, SecretKey, Signature};
use sha2::{Digest, Sha256};

/// Sign the chain hash every N frames (~1 second at 50fps Opus).
pub const CHECKPOINT_INTERVAL: u64 = 50;

/// Frames a listener accepts after the last verified checkpoint (or after the start of
/// the stream) before it rejects the upstream: one interval plus 0.5 s.
pub const MAX_FRAMES_WITHOUT_CHECKPOINT: u64 = CHECKPOINT_INTERVAL + 25;

/// Sequence gaps a listener tolerates in a row before it rejects the upstream.
pub const MAX_RESYNCS: u32 = 3;

/// Frames after which one tolerated gap is restored (10 s).
pub const RESYNC_REFILL_FRAMES: u64 = 500;

/// Byte size of the checkpoint auth extension prepended to the Opus payload:
/// sequence(8) + chain_hash(32) + signature(64).
pub const CHECKPOINT_EXTRA: usize = 104;

const CHECKPOINT_DOMAIN_TAG: &[u8] = b"proscenium/stage-checkpoint/2";

/// chain_hash <- SHA256(chain_hash || seq || timestamp || opus_payload)
fn extend_chain(chain_hash: &[u8; 32], seq: u32, timestamp: u32, opus: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(chain_hash);
    hasher.update(seq.to_be_bytes());
    hasher.update(timestamp.to_be_bytes());
    hasher.update(opus);
    hasher.finalize().into()
}

/// Bytes the host signs at a checkpoint: domain tag, stage id, checkpoint sequence
/// number and chain hash. The stage id is a fixed-length hex string, so the NUL
/// separators make the encoding unambiguous.
fn checkpoint_signing_bytes(stage_id: &str, sequence: u64, chain_hash: &[u8; 32]) -> Vec<u8> {
    let mut buf =
        Vec::with_capacity(CHECKPOINT_DOMAIN_TAG.len() + stage_id.len() + 2 + 8 + chain_hash.len());
    buf.extend_from_slice(CHECKPOINT_DOMAIN_TAG);
    buf.push(0);
    buf.extend_from_slice(stage_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&sequence.to_be_bytes());
    buf.extend_from_slice(chain_hash);
    buf
}

// ---- HostAuthState ------------------------------------------------------

/// Maintained by the HostMixer to authenticate each outgoing mixed frame.
///
/// The chain hash covers the header fields `seq` and `timestamp` and the Opus payload of
/// every frame. Every CHECKPOINT_INTERVAL frames the host signs the chain hash together
/// with the stage id and the frame count.
pub struct HostAuthState {
    chain_hash: [u8; 32],
    /// Number of frames processed since the stage started.
    sequence: u64,
    signing_key: SecretKey,
    stage_id: String,
}

impl HostAuthState {
    pub fn new(signing_key: SecretKey, stage_id: String) -> Self {
        Self {
            chain_hash: [0u8; 32],
            sequence: 0,
            signing_key,
            stage_id,
        }
    }

    /// Process one Opus frame. Returns `(tag_byte, wire_payload)`:
    ///   Normal:     (TAG_NORMAL,      opus_bytes)
    ///   Checkpoint: (TAG_CHECKPOINT,  sequence(8) + hash(32) + sig(64) + opus_bytes)
    pub fn process(&mut self, seq: u32, timestamp: u32, opus: &[u8]) -> (u8, Vec<u8>) {
        self.chain_hash = extend_chain(&self.chain_hash, seq, timestamp, opus);
        self.sequence += 1;

        if self.sequence.is_multiple_of(CHECKPOINT_INTERVAL) {
            let sig: Signature = self.signing_key.sign(&checkpoint_signing_bytes(
                &self.stage_id,
                self.sequence,
                &self.chain_hash,
            ));
            let mut buf = Vec::with_capacity(CHECKPOINT_EXTRA + opus.len());
            buf.extend_from_slice(&self.sequence.to_be_bytes());
            buf.extend_from_slice(&self.chain_hash);
            buf.extend_from_slice(&sig.to_bytes());
            buf.extend_from_slice(opus);
            (TAG_CHECKPOINT, buf)
        } else {
            (TAG_NORMAL, opus.to_vec())
        }
    }
}

// ---- ListenerAuthState --------------------------------------------------

/// Checkpoint data carried by a TAG_CHECKPOINT frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// Host frame count at this checkpoint.
    pub sequence: u64,
    pub chain_hash: [u8; 32],
    pub signature: [u8; 64],
}

/// Reason a listener stops playing a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// The chain hash computed from the received frames differs from the signed one.
    Tampered,
    /// The checkpoint is not signed by the host key for this stage.
    InvalidSignature,
    /// More than `MAX_FRAMES_WITHOUT_CHECKPOINT` frames arrived without a checkpoint.
    CheckpointMissing,
    /// The checkpoint sequence number is not above the last verified one.
    Replayed,
    /// More sequence gaps than the tolerated number.
    TooManyGaps,
    /// Unknown tag byte, or a checkpoint tag without checkpoint data.
    Malformed,
}

impl AuthFailure {
    /// Identifier reported to the frontend in `StageEvent::AuthFailed`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tampered => "tamper_detected",
            Self::InvalidSignature => "invalid_signature",
            Self::CheckpointMissing => "checkpoint_missing",
            Self::Replayed => "replayed",
            Self::TooManyGaps => "too_many_gaps",
            Self::Malformed => "malformed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    /// Frame extends a verified chain; the next checkpoint covers it.
    Ok,
    /// No verified chain yet (start of the stream or after a sequence gap).
    Unverified,
    /// Checkpoint valid: all frames since the previous checkpoint are authentic.
    Verified,
    /// The stream is rejected. Every later frame returns the same failure.
    Failed(AuthFailure),
}

/// Maintained by the listener to verify the incoming mixed stream.
///
/// A listener that joins mid-stream, or that detects a gap in the frame sequence (the
/// sender skipped frames for a slow subscriber), has no chain state. It then adopts the
/// chain hash of the next signed checkpoint and verifies the chain from there.
/// Frames received without a verified chain are bounded by `MAX_FRAMES_WITHOUT_CHECKPOINT`
/// and gaps by `MAX_RESYNCS`.
pub struct ListenerAuthState {
    host_pubkey: PublicKey,
    stage_id: String,
    /// Chain hash after the last received frame; `None` while there is no verified chain.
    chain_hash: Option<[u8; 32]>,
    /// Frames received since the last verified checkpoint or since the stream started.
    frames_since_checkpoint: u64,
    /// Header `seq` of the last frame.
    last_frame_seq: Option<u32>,
    /// Sequence number of the last verified checkpoint (0 if none).
    checkpoint_floor: u64,
    /// Sequence gaps still tolerated.
    resync_budget: u32,
    frames_since_refill: u64,
    failure: Option<AuthFailure>,
}

impl ListenerAuthState {
    /// `checkpoint_floor` is the highest checkpoint sequence number already verified for
    /// this stage (0 if none); a checkpoint at or below it is rejected as a replay.
    pub fn new(host_pubkey: PublicKey, stage_id: String, checkpoint_floor: u64) -> Self {
        Self {
            host_pubkey,
            stage_id,
            chain_hash: None,
            frames_since_checkpoint: 0,
            last_frame_seq: None,
            checkpoint_floor,
            resync_budget: MAX_RESYNCS,
            frames_since_refill: 0,
            failure: None,
        }
    }

    /// Sequence number of the last verified checkpoint (0 if none).
    pub fn checkpoint_floor(&self) -> u64 {
        self.checkpoint_floor
    }

    fn fail(&mut self, failure: AuthFailure) -> AuthResult {
        self.failure = Some(failure);
        AuthResult::Failed(failure)
    }

    /// Verify one incoming frame.
    ///
    /// `seq`, `timestamp`, `tag` are the frame header fields.
    /// `opus`       -- raw Opus bytes (checkpoint prefix already stripped).
    /// `checkpoint` -- the checkpoint prefix when `tag` is TAG_CHECKPOINT.
    pub fn verify_frame(
        &mut self,
        seq: u32,
        timestamp: u32,
        tag: u8,
        opus: &[u8],
        checkpoint: Option<&Checkpoint>,
    ) -> AuthResult {
        if let Some(failure) = self.failure {
            return AuthResult::Failed(failure);
        }

        // A sequence gap means frames are missing, so the local chain cannot match the
        // host's. Discard it and wait for the next checkpoint.
        if let Some(prev) = self.last_frame_seq
            && seq != prev.wrapping_add(1)
        {
            self.chain_hash = None;
            if self.resync_budget == 0 {
                return self.fail(AuthFailure::TooManyGaps);
            }
            self.resync_budget -= 1;
        }
        self.last_frame_seq = Some(seq);

        self.frames_since_refill += 1;
        if self.frames_since_refill >= RESYNC_REFILL_FRAMES {
            self.frames_since_refill = 0;
            self.resync_budget = (self.resync_budget + 1).min(MAX_RESYNCS);
        }

        self.frames_since_checkpoint += 1;
        if let Some(chain) = self.chain_hash {
            self.chain_hash = Some(extend_chain(&chain, seq, timestamp, opus));
        }

        match (tag, checkpoint) {
            (TAG_NORMAL, _) => {
                if self.frames_since_checkpoint > MAX_FRAMES_WITHOUT_CHECKPOINT {
                    return self.fail(AuthFailure::CheckpointMissing);
                }
                if self.chain_hash.is_some() {
                    AuthResult::Ok
                } else {
                    AuthResult::Unverified
                }
            }
            (TAG_CHECKPOINT, Some(cp)) => {
                let signed = checkpoint_signing_bytes(&self.stage_id, cp.sequence, &cp.chain_hash);
                let sig = Signature::from_bytes(&cp.signature);
                if self.host_pubkey.verify(&signed, &sig).is_err() {
                    return self.fail(AuthFailure::InvalidSignature);
                }
                if cp.sequence <= self.checkpoint_floor {
                    return self.fail(AuthFailure::Replayed);
                }
                if let Some(chain) = self.chain_hash
                    && chain != cp.chain_hash
                {
                    return self.fail(AuthFailure::Tampered);
                }
                self.chain_hash = Some(cp.chain_hash);
                self.checkpoint_floor = cp.sequence;
                self.frames_since_checkpoint = 0;
                AuthResult::Verified
            }
            _ => self.fail(AuthFailure::Malformed),
        }
    }
}

// ---- Wire format helper -------------------------------------------------

/// Decode a checkpoint payload: `[sequence:8][chain_hash:32][signature:64][opus...]`
///
/// Returns the checkpoint and the Opus bytes.
pub fn decode_checkpoint_payload(payload: &[u8]) -> Result<(Checkpoint, &[u8]), AppError> {
    let too_short = || {
        AppError::Other(format!(
            "checkpoint payload too short: {} bytes",
            payload.len()
        ))
    };
    let (sequence, rest) = payload.split_first_chunk::<8>().ok_or_else(too_short)?;
    let (chain_hash, rest) = rest.split_first_chunk::<32>().ok_or_else(too_short)?;
    let (signature, opus) = rest.split_first_chunk::<64>().ok_or_else(too_short)?;
    Ok((
        Checkpoint {
            sequence: u64::from_be_bytes(*sequence),
            chain_hash: *chain_hash,
            signature: *signature,
        },
        opus,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE_ID: &str = "000102030405060708090a0b0c0d0e0f";

    struct Frame {
        seq: u32,
        timestamp: u32,
        tag: u8,
        payload: Vec<u8>,
    }

    /// Host side: produces the frames the mixer would put on the wire.
    struct Host {
        auth: HostAuthState,
        seq: u32,
    }

    impl Host {
        fn new(key_seed: u8, stage_id: &str) -> Self {
            Self {
                auth: HostAuthState::new(
                    SecretKey::from_bytes(&[key_seed; 32]),
                    stage_id.to_string(),
                ),
                seq: 0,
            }
        }

        fn public(key_seed: u8) -> PublicKey {
            SecretKey::from_bytes(&[key_seed; 32]).public()
        }

        fn next(&mut self) -> Frame {
            let opus = vec![self.seq as u8; 20];
            let timestamp = self.seq.wrapping_mul(960);
            let (tag, payload) = self.auth.process(self.seq, timestamp, &opus);
            let frame = Frame {
                seq: self.seq,
                timestamp,
                tag,
                payload,
            };
            self.seq += 1;
            frame
        }

        fn frames(&mut self, count: usize) -> Vec<Frame> {
            (0..count).map(|_| self.next()).collect()
        }
    }

    fn listener(floor: u64) -> ListenerAuthState {
        ListenerAuthState::new(Host::public(1), STAGE_ID.to_string(), floor)
    }

    fn feed(auth: &mut ListenerAuthState, frame: &Frame) -> AuthResult {
        if frame.tag == TAG_CHECKPOINT {
            let (checkpoint, opus) = decode_checkpoint_payload(&frame.payload).unwrap();
            auth.verify_frame(
                frame.seq,
                frame.timestamp,
                frame.tag,
                opus,
                Some(&checkpoint),
            )
        } else {
            auth.verify_frame(frame.seq, frame.timestamp, frame.tag, &frame.payload, None)
        }
    }

    fn feed_all(auth: &mut ListenerAuthState, frames: &[Frame]) -> Vec<AuthResult> {
        frames.iter().map(|f| feed(auth, f)).collect()
    }

    #[test]
    fn checkpoint_every_interval_and_chain_verifies() {
        let mut host = Host::new(1, STAGE_ID);
        let frames = host.frames(150);
        let checkpoints: Vec<usize> = frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.tag == TAG_CHECKPOINT)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(checkpoints, vec![49, 99, 149]);

        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        // No chain before the first checkpoint, verified chain afterwards.
        assert!(results[..49].iter().all(|r| *r == AuthResult::Unverified));
        assert_eq!(results[49], AuthResult::Verified);
        assert!(results[50..99].iter().all(|r| *r == AuthResult::Ok));
        assert_eq!(results[99], AuthResult::Verified);
        assert_eq!(results[149], AuthResult::Verified);
        assert_eq!(auth.checkpoint_floor(), 150);
    }

    #[test]
    fn late_joiner_adopts_first_checkpoint() {
        let mut host = Host::new(1, STAGE_ID);
        let frames = host.frames(200);
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames[70..]);
        assert_eq!(results[0], AuthResult::Unverified);
        assert_eq!(results[29], AuthResult::Verified);
        assert_eq!(results[79], AuthResult::Verified);
    }

    #[test]
    fn modified_payload_fails_at_next_checkpoint_and_stays_failed() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(150);
        frames[60].payload[0] ^= 0xff;
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(results[49], AuthResult::Verified);
        assert_eq!(results[60], AuthResult::Ok);
        assert_eq!(results[99], AuthResult::Failed(AuthFailure::Tampered));
        assert!(
            results[100..]
                .iter()
                .all(|r| *r == AuthResult::Failed(AuthFailure::Tampered))
        );
    }

    #[test]
    fn modified_timestamp_header_fails_at_next_checkpoint() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(100);
        frames[60].timestamp ^= 1;
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(results[99], AuthResult::Failed(AuthFailure::Tampered));
    }

    #[test]
    fn stream_without_checkpoints_is_rejected_after_the_frame_limit() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(200);
        // A relay that never forwards a checkpoint frame as such.
        for frame in &mut frames {
            if frame.tag == TAG_CHECKPOINT {
                frame.tag = TAG_NORMAL;
                frame.payload.drain(..CHECKPOINT_EXTRA);
            }
        }
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        let limit = MAX_FRAMES_WITHOUT_CHECKPOINT as usize;
        assert!(
            results[..limit]
                .iter()
                .all(|r| *r == AuthResult::Unverified)
        );
        assert_eq!(
            results[limit],
            AuthResult::Failed(AuthFailure::CheckpointMissing)
        );
    }

    #[test]
    fn checkpoints_stripped_after_verification_are_rejected_after_the_frame_limit() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(200);
        for frame in frames.iter_mut().skip(50) {
            if frame.tag == TAG_CHECKPOINT {
                frame.tag = TAG_NORMAL;
                frame.payload.drain(..CHECKPOINT_EXTRA);
            }
        }
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(results[49], AuthResult::Verified);
        let limit = 50 + MAX_FRAMES_WITHOUT_CHECKPOINT as usize;
        assert!(results[50..limit].iter().all(|r| *r == AuthResult::Ok));
        assert_eq!(
            results[limit],
            AuthResult::Failed(AuthFailure::CheckpointMissing)
        );
    }

    #[test]
    fn checkpoint_signed_by_another_key_or_for_another_stage_is_rejected() {
        let mut other_key = Host::new(2, STAGE_ID);
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &other_key.frames(50));
        assert_eq!(
            results[49],
            AuthResult::Failed(AuthFailure::InvalidSignature)
        );

        let mut other_stage = Host::new(1, "ff0102030405060708090a0b0c0d0e0f");
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &other_stage.frames(50));
        assert_eq!(
            results[49],
            AuthResult::Failed(AuthFailure::InvalidSignature)
        );
    }

    #[test]
    fn recorded_stream_below_the_verified_sequence_is_rejected() {
        let mut host = Host::new(1, STAGE_ID);
        let recorded = host.frames(100);

        // The listener verified up to sequence 100 on another connection.
        let mut auth = listener(100);
        let results = feed_all(&mut auth, &recorded);
        assert_eq!(results[49], AuthResult::Failed(AuthFailure::Replayed));

        // The live stream continues above the floor.
        let mut auth = listener(100);
        let results = feed_all(&mut auth, &host.frames(50));
        assert_eq!(results[49], AuthResult::Verified);
        assert_eq!(auth.checkpoint_floor(), 150);
    }

    #[test]
    fn gap_resets_to_unverified_and_next_checkpoint_is_adopted() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(200);
        // The sender skipped frames 60..70 for a slow subscriber.
        frames.drain(60..70);
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(results[49], AuthResult::Verified);
        assert_eq!(results[59], AuthResult::Ok);
        // First frame after the gap (seq 70) up to the checkpoint at seq 99.
        assert!(results[60..89].iter().all(|r| *r == AuthResult::Unverified));
        assert_eq!(results[89], AuthResult::Verified);
        assert!(results[90..139].iter().all(|r| *r == AuthResult::Ok));
        assert_eq!(results[139], AuthResult::Verified);
    }

    #[test]
    fn repeated_gaps_exhaust_the_budget() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(300);
        // Four gaps within less than RESYNC_REFILL_FRAMES frames.
        for start in [230, 170, 110, 60] {
            frames.drain(start..start + 2);
        }
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        let failed_at = results
            .iter()
            .position(|r| matches!(r, AuthResult::Failed(_)))
            .unwrap();
        assert_eq!(
            results[failed_at],
            AuthResult::Failed(AuthFailure::TooManyGaps)
        );
        // The first three gaps are tolerated; the fourth is at original index 230.
        assert_eq!(frames[failed_at].seq, 232);
    }

    #[test]
    fn gap_budget_is_restored_over_time() {
        let mut host = Host::new(1, STAGE_ID);
        let mut auth = listener(0);
        // One gap every RESYNC_REFILL_FRAMES frames never exhausts the budget.
        for _ in 0..(2 * MAX_RESYNCS + 2) {
            let frames = host.frames(RESYNC_REFILL_FRAMES as usize);
            let results = feed_all(&mut auth, &frames);
            assert!(!results.iter().any(|r| matches!(r, AuthResult::Failed(_))));
            host.next(); // skipped frame
        }
    }

    #[test]
    fn checkpoint_tag_without_data_and_unknown_tag_are_rejected() {
        let mut auth = listener(0);
        assert_eq!(
            auth.verify_frame(0, 0, TAG_CHECKPOINT, &[1, 2, 3], None),
            AuthResult::Failed(AuthFailure::Malformed)
        );
        let mut auth = listener(0);
        assert_eq!(
            auth.verify_frame(0, 0, 0x7f, &[1, 2, 3], None),
            AuthResult::Failed(AuthFailure::Malformed)
        );
    }

    #[test]
    fn decode_checkpoint_payload_checks_length() {
        assert!(decode_checkpoint_payload(&[0u8; CHECKPOINT_EXTRA - 1]).is_err());
        let mut payload = vec![0u8; CHECKPOINT_EXTRA];
        payload[7] = 9;
        payload.extend_from_slice(b"opus");
        let (checkpoint, opus) = decode_checkpoint_payload(&payload).unwrap();
        assert_eq!(checkpoint.sequence, 9);
        assert_eq!(opus, b"opus");
    }
}
