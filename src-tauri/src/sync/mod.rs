mod client;
mod orchestration;
mod processing;
mod server;

pub use orchestration::sync_one_peer;
pub use server::handle_sync;

/// Interaction rule of the sync protocol, for a request whose interaction count differs
/// from the responder's: `true` if the responder sends only the interactions after the
/// requester's newest interaction timestamp, `false` if it sends every interaction.
///
/// The responder evaluates it to select the rows. The requester evaluates it to
/// determine whether the frames carry the author's complete interaction set.
fn sends_only_newer_interactions(
    responder_count: u64,
    requester_count: u64,
    responder_count_after_newest: u64,
) -> bool {
    responder_count.checked_sub(requester_count) == Some(responder_count_after_newest)
}

#[cfg(test)]
mod tests {
    use super::sends_only_newer_interactions;

    #[test]
    fn only_newer_interactions_when_they_account_for_the_difference() {
        // The responder has 3 more, all after the requester's newest timestamp.
        assert!(sends_only_newer_interactions(10, 7, 3));
        // The responder has 3 more but only 2 are newer: an older one is missing.
        assert!(!sends_only_newer_interactions(10, 7, 2));
        // The requester has more than the responder: some were deleted by the author.
        assert!(!sends_only_newer_interactions(5, 7, 0));
        assert!(!sends_only_newer_interactions(0, u64::MAX, 0));
    }
}
