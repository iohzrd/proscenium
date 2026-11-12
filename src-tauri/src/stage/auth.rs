use crate::audio::{TAG_CHECKPOINT, TAG_NORMAL};
use crate::error::AppError;
use iroh::{PublicKey, SecretKey, Signature};
use sha2::{Digest, Sha256};

/// Sign the chain hash every N frames (200 ms at 50 fps Opus). A listener plays a frame
/// only after the checkpoint that covers it verified, so this is the added delay.
pub const CHECKPOINT_INTERVAL: u64 = 10;

/// Frames a listener accepts after the last verified checkpoint (or after the start of
/// the stream) before it rejects the upstream: one interval plus 0.5 s.
pub const MAX_FRAMES_WITHOUT_CHECKPOINT: u64 = CHECKPOINT_INTERVAL + 25;

/// Sequence gaps a listener tolerates in a row before it rejects the upstream.
pub const MAX_RESYNCS: u32 = 3;

/// Frames after which one tolerated gap is restored (10 s).
pub const RESYNC_REFILL_FRAMES: u64 = 500;

/// Largest difference between the host time in a checkpoint and the listener's clock.
/// A recording older than this is rejected, whatever the listener verified before.
pub const MAX_CHECKPOINT_SKEW_MS: u64 = 60_000;

/// Byte size of the checkpoint auth extension prepended to the Opus payload:
/// sequence(8) + host time(8) + chain_hash(32) + signature(64).
pub const CHECKPOINT_EXTRA: usize = 112;

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
/// number, host time and chain hash. The stage id is a fixed-length hex string, so the
/// NUL separators make the encoding unambiguous.
fn checkpoint_signing_bytes(
    stage_id: &str,
    sequence: u64,
    time_ms: u64,
    chain_hash: &[u8; 32],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(
        CHECKPOINT_DOMAIN_TAG.len() + stage_id.len() + 2 + 16 + chain_hash.len(),
    );
    buf.extend_from_slice(CHECKPOINT_DOMAIN_TAG);
    buf.push(0);
    buf.extend_from_slice(stage_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&sequence.to_be_bytes());
    buf.extend_from_slice(&time_ms.to_be_bytes());
    buf.extend_from_slice(chain_hash);
    buf
}

// ---- HostAuthState ------------------------------------------------------

/// Maintained by the HostMixer to authenticate each outgoing mixed frame.
///
/// The chain hash covers the header fields `seq` and `timestamp` and the Opus payload of
/// every frame. Every CHECKPOINT_INTERVAL frames the host signs the chain hash together
/// with the stage id, the frame count and the host time.
pub struct HostAuthState {
    chain_hash: [u8; 32],
    /// Number of frames processed since the stage started.
    sequence: u64,
    /// The host's master key.
    master_key: SecretKey,
    stage_id: String,
}

impl HostAuthState {
    pub fn new(master_key: SecretKey, stage_id: String) -> Self {
        Self {
            chain_hash: [0u8; 32],
            sequence: 0,
            master_key,
            stage_id,
        }
    }

    /// Process one Opus frame at host time `now_ms`. Returns `(tag_byte, wire_payload)`:
    ///   Normal:     (TAG_NORMAL,      opus_bytes)
    ///   Checkpoint: (TAG_CHECKPOINT,  sequence(8) + time(8) + hash(32) + sig(64) + opus)
    pub fn process(&mut self, seq: u32, timestamp: u32, opus: &[u8], now_ms: u64) -> (u8, Vec<u8>) {
        self.chain_hash = extend_chain(&self.chain_hash, seq, timestamp, opus);
        self.sequence += 1;

        if self.sequence.is_multiple_of(CHECKPOINT_INTERVAL) {
            let sig: Signature = self.master_key.sign(&checkpoint_signing_bytes(
                &self.stage_id,
                self.sequence,
                now_ms,
                &self.chain_hash,
            ));
            let mut buf = Vec::with_capacity(CHECKPOINT_EXTRA + opus.len());
            buf.extend_from_slice(&self.sequence.to_be_bytes());
            buf.extend_from_slice(&now_ms.to_be_bytes());
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
    /// Host wall clock time (Unix ms) at this checkpoint.
    pub time_ms: u64,
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
    /// The checkpoint sequence number is not above the last verified one, its host time
    /// is earlier than the previous checkpoint's, or it differs from the local clock by
    /// more than `MAX_CHECKPOINT_SKEW_MS`.
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

/// What the listener does with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    /// The frame extends a chain that starts at a verified checkpoint: hold it until the
    /// next checkpoint.
    Hold,
    /// No verified chain covers the frame (start of the stream, after a sequence gap, or
    /// a checkpoint whose chain hash was adopted without the frames before it): discard it
    /// and the held frames.
    Discard,
    /// Valid checkpoint that extends the verified chain: the held frames and this one are
    /// authentic; play them.
    Release,
    /// The stream is rejected. Every later frame returns the same failure.
    Failed(AuthFailure),
}

/// Maintained by the listener to verify the incoming mixed stream.
///
/// A listener that joins mid-stream, or that detects a gap in the frame sequence (the
/// sender skipped frames for a slow subscriber), has no chain state. It then adopts the
/// chain hash of the next signed checkpoint and verifies the frames after it at the
/// checkpoint after that. Frames without a verified chain are never played. Frames
/// received without a verified checkpoint are bounded by `MAX_FRAMES_WITHOUT_CHECKPOINT`
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
    /// Host time of the last verified checkpoint of this stream (0 if none).
    last_checkpoint_time: u64,
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
            last_checkpoint_time: 0,
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

    /// Verify one incoming frame, received at local time `now_ms`.
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
        now_ms: u64,
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
                    AuthResult::Hold
                } else {
                    AuthResult::Discard
                }
            }
            (TAG_CHECKPOINT, Some(cp)) => {
                let signed = checkpoint_signing_bytes(
                    &self.stage_id,
                    cp.sequence,
                    cp.time_ms,
                    &cp.chain_hash,
                );
                let sig = Signature::from_bytes(&cp.signature);
                if self.host_pubkey.verify(&signed, &sig).is_err() {
                    return self.fail(AuthFailure::InvalidSignature);
                }
                if cp.sequence <= self.checkpoint_floor
                    || cp.time_ms < self.last_checkpoint_time
                    || cp.time_ms.abs_diff(now_ms) > MAX_CHECKPOINT_SKEW_MS
                {
                    return self.fail(AuthFailure::Replayed);
                }
                let result = match self.chain_hash {
                    Some(chain) if chain != cp.chain_hash => {
                        return self.fail(AuthFailure::Tampered);
                    }
                    Some(_) => AuthResult::Release,
                    None => AuthResult::Discard,
                };
                self.chain_hash = Some(cp.chain_hash);
                self.checkpoint_floor = cp.sequence;
                self.last_checkpoint_time = cp.time_ms;
                self.frames_since_checkpoint = 0;
                result
            }
            _ => self.fail(AuthFailure::Malformed),
        }
    }
}

// ---- Wire format helper -------------------------------------------------

/// Decode a checkpoint payload:
/// `[sequence:8][time:8][chain_hash:32][signature:64][opus...]`
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
    let (time_ms, rest) = rest.split_first_chunk::<8>().ok_or_else(too_short)?;
    let (chain_hash, rest) = rest.split_first_chunk::<32>().ok_or_else(too_short)?;
    let (signature, opus) = rest.split_first_chunk::<64>().ok_or_else(too_short)?;
    Ok((
        Checkpoint {
            sequence: u64::from_be_bytes(*sequence),
            time_ms: u64::from_be_bytes(*time_ms),
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
    /// Host time of the first frame.
    const START_MS: u64 = 1_700_000_000_000;
    const INTERVAL: usize = CHECKPOINT_INTERVAL as usize;

    struct Frame {
        seq: u32,
        timestamp: u32,
        tag: u8,
        payload: Vec<u8>,
        /// Host time when the frame was produced; the listener receives it at this time.
        time_ms: u64,
    }

    /// Host side: produces the frames the mixer would put on the wire.
    struct Host {
        auth: HostAuthState,
        seq: u32,
        time_ms: u64,
    }

    impl Host {
        fn new(key_seed: u8, stage_id: &str) -> Self {
            Self {
                auth: HostAuthState::new(
                    SecretKey::from_bytes(&[key_seed; 32]),
                    stage_id.to_string(),
                ),
                seq: 0,
                time_ms: START_MS,
            }
        }

        fn public(key_seed: u8) -> PublicKey {
            SecretKey::from_bytes(&[key_seed; 32]).public()
        }

        fn next(&mut self) -> Frame {
            let opus = vec![self.seq as u8; 20];
            let timestamp = self.seq.wrapping_mul(960);
            let (tag, payload) = self.auth.process(self.seq, timestamp, &opus, self.time_ms);
            let frame = Frame {
                seq: self.seq,
                timestamp,
                tag,
                payload,
                time_ms: self.time_ms,
            };
            self.seq += 1;
            self.time_ms += 20;
            frame
        }

        fn frames(&mut self, count: usize) -> Vec<Frame> {
            (0..count).map(|_| self.next()).collect()
        }
    }

    fn listener(floor: u64) -> ListenerAuthState {
        ListenerAuthState::new(Host::public(1), STAGE_ID.to_string(), floor)
    }

    /// Feed `frame` as received `delay_ms` after the host produced it.
    fn feed_at(auth: &mut ListenerAuthState, frame: &Frame, delay_ms: u64) -> AuthResult {
        let now = frame.time_ms + delay_ms;
        if frame.tag == TAG_CHECKPOINT {
            let (checkpoint, opus) = decode_checkpoint_payload(&frame.payload).unwrap();
            auth.verify_frame(
                frame.seq,
                frame.timestamp,
                frame.tag,
                opus,
                Some(&checkpoint),
                now,
            )
        } else {
            let payload = &frame.payload;
            auth.verify_frame(frame.seq, frame.timestamp, frame.tag, payload, None, now)
        }
    }

    fn feed_all(auth: &mut ListenerAuthState, frames: &[Frame]) -> Vec<AuthResult> {
        frames.iter().map(|f| feed_at(auth, f, 0)).collect()
    }

    fn all(results: &[AuthResult], expected: AuthResult) -> bool {
        results.iter().all(|r| *r == expected)
    }

    #[test]
    fn frames_are_released_only_by_a_checkpoint_that_extends_the_verified_chain() {
        let mut host = Host::new(1, STAGE_ID);
        let frames = host.frames(3 * INTERVAL);
        let checkpoints: Vec<usize> = frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.tag == TAG_CHECKPOINT)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            checkpoints,
            vec![INTERVAL - 1, 2 * INTERVAL - 1, 3 * INTERVAL - 1]
        );

        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        // Before and at the first checkpoint there is no verified chain: nothing plays.
        assert!(all(&results[..INTERVAL], AuthResult::Discard));
        // The frames after it are held until the next checkpoint releases them.
        assert!(all(&results[INTERVAL..2 * INTERVAL - 1], AuthResult::Hold));
        assert_eq!(results[2 * INTERVAL - 1], AuthResult::Release);
        assert_eq!(results[3 * INTERVAL - 1], AuthResult::Release);
        assert_eq!(auth.checkpoint_floor(), 3 * CHECKPOINT_INTERVAL);
    }

    #[test]
    fn late_joiner_adopts_the_first_checkpoint_it_receives() {
        let mut host = Host::new(1, STAGE_ID);
        let frames = host.frames(5 * INTERVAL);
        let mut auth = listener(0);
        let joined = 2 * INTERVAL + 3;
        let results = feed_all(&mut auth, &frames[joined..]);
        let first = 3 * INTERVAL - 1 - joined;
        assert!(all(&results[..=first], AuthResult::Discard));
        assert!(all(&results[first + 1..first + INTERVAL], AuthResult::Hold));
        assert_eq!(results[first + INTERVAL], AuthResult::Release);
    }

    #[test]
    fn modified_payload_is_never_released_and_fails_the_stream() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(4 * INTERVAL);
        frames[INTERVAL + 5].payload[0] ^= 0xff;
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(results[INTERVAL + 5], AuthResult::Hold);
        assert_eq!(
            results[2 * INTERVAL - 1],
            AuthResult::Failed(AuthFailure::Tampered)
        );
        assert!(all(
            &results[2 * INTERVAL..],
            AuthResult::Failed(AuthFailure::Tampered)
        ));
    }

    #[test]
    fn modified_timestamp_header_fails_at_next_checkpoint() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(2 * INTERVAL);
        frames[INTERVAL + 2].timestamp ^= 1;
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(
            results[2 * INTERVAL - 1],
            AuthResult::Failed(AuthFailure::Tampered)
        );
    }

    #[test]
    fn stream_without_checkpoints_is_rejected_after_the_frame_limit() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(100);
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
        assert!(all(&results[..limit], AuthResult::Discard));
        assert_eq!(
            results[limit],
            AuthResult::Failed(AuthFailure::CheckpointMissing)
        );
    }

    #[test]
    fn checkpoints_stripped_after_verification_are_rejected_after_the_frame_limit() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(100);
        for frame in frames.iter_mut().skip(INTERVAL) {
            if frame.tag == TAG_CHECKPOINT {
                frame.tag = TAG_NORMAL;
                frame.payload.drain(..CHECKPOINT_EXTRA);
            }
        }
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        let limit = INTERVAL + MAX_FRAMES_WITHOUT_CHECKPOINT as usize;
        assert!(all(&results[INTERVAL..limit], AuthResult::Hold));
        assert_eq!(
            results[limit],
            AuthResult::Failed(AuthFailure::CheckpointMissing)
        );
    }

    #[test]
    fn checkpoint_signed_by_another_key_or_for_another_stage_is_rejected() {
        let mut other_key = Host::new(2, STAGE_ID);
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &other_key.frames(INTERVAL));
        assert_eq!(
            results[INTERVAL - 1],
            AuthResult::Failed(AuthFailure::InvalidSignature)
        );

        let mut other_stage = Host::new(1, "ff0102030405060708090a0b0c0d0e0f");
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &other_stage.frames(INTERVAL));
        assert_eq!(
            results[INTERVAL - 1],
            AuthResult::Failed(AuthFailure::InvalidSignature)
        );
    }

    #[test]
    fn recorded_stream_below_the_verified_sequence_is_rejected() {
        let mut host = Host::new(1, STAGE_ID);
        let recorded = host.frames(2 * INTERVAL);

        // The listener verified up to sequence 20 on another connection.
        let floor = 2 * CHECKPOINT_INTERVAL;
        let mut auth = listener(floor);
        let results = feed_all(&mut auth, &recorded);
        assert_eq!(
            results[INTERVAL - 1],
            AuthResult::Failed(AuthFailure::Replayed)
        );

        // The live stream continues above the floor.
        let mut auth = listener(floor);
        let results = feed_all(&mut auth, &host.frames(INTERVAL));
        assert_eq!(results[INTERVAL - 1], AuthResult::Discard);
        assert_eq!(auth.checkpoint_floor(), floor + CHECKPOINT_INTERVAL);
    }

    #[test]
    fn recording_older_than_the_skew_limit_is_rejected_by_a_new_listener() {
        let mut host = Host::new(1, STAGE_ID);
        let frames = host.frames(INTERVAL);
        let mut auth = listener(0);
        let late = MAX_CHECKPOINT_SKEW_MS + 1;
        let results: Vec<_> = frames.iter().map(|f| feed_at(&mut auth, f, late)).collect();
        assert_eq!(
            results[INTERVAL - 1],
            AuthResult::Failed(AuthFailure::Replayed)
        );

        // Within the limit the checkpoint is accepted.
        let mut auth = listener(0);
        let on_time = MAX_CHECKPOINT_SKEW_MS;
        let results: Vec<_> = frames
            .iter()
            .map(|f| feed_at(&mut auth, f, on_time))
            .collect();
        assert_eq!(results[INTERVAL - 1], AuthResult::Discard);
    }

    #[test]
    fn checkpoint_with_an_earlier_host_time_is_rejected() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(INTERVAL);
        host.time_ms -= 1_000;
        frames.extend(host.frames(INTERVAL));
        let mut auth = listener(0);
        let results: Vec<_> = frames.iter().map(|f| feed_at(&mut auth, f, 0)).collect();
        assert_eq!(results[INTERVAL - 1], AuthResult::Discard);
        assert_eq!(
            results[2 * INTERVAL - 1],
            AuthResult::Failed(AuthFailure::Replayed)
        );
    }

    #[test]
    fn gap_discards_until_the_checkpoint_after_the_adopted_one() {
        let mut host = Host::new(1, STAGE_ID);
        let mut frames = host.frames(6 * INTERVAL);
        // The sender skipped frames 25 and 26 for a slow subscriber.
        frames.drain(25..27);
        let mut auth = listener(0);
        let results = feed_all(&mut auth, &frames);
        assert_eq!(results[19], AuthResult::Release);
        assert!(all(&results[20..25], AuthResult::Hold));
        // Seq 27 and 28 follow the gap; seq 29 is the checkpoint that is adopted.
        assert!(all(&results[25..=27], AuthResult::Discard));
        assert!(all(&results[28..37], AuthResult::Hold));
        assert_eq!(results[37], AuthResult::Release);
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
            auth.verify_frame(0, 0, TAG_CHECKPOINT, &[1, 2, 3], None, START_MS),
            AuthResult::Failed(AuthFailure::Malformed)
        );
        let mut auth = listener(0);
        assert_eq!(
            auth.verify_frame(0, 0, 0x7f, &[1, 2, 3], None, START_MS),
            AuthResult::Failed(AuthFailure::Malformed)
        );
    }

    #[test]
    fn decode_checkpoint_payload_checks_length() {
        assert!(decode_checkpoint_payload(&[0u8; CHECKPOINT_EXTRA - 1]).is_err());
        let mut payload = vec![0u8; CHECKPOINT_EXTRA];
        payload[7] = 9;
        payload[15] = 5;
        payload.extend_from_slice(b"opus");
        let (checkpoint, opus) = decode_checkpoint_payload(&payload).unwrap();
        assert_eq!(checkpoint.sequence, 9);
        assert_eq!(checkpoint.time_ms, 5);
        assert_eq!(opus, b"opus");
    }
}
