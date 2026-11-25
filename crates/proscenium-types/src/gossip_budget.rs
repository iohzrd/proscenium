//! Per-sender message budgets of one gossip topic, used by the client and the server.

use iroh::EndpointId;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Messages one sender may deliver on a topic at once, and on average per second, before
/// its further messages are discarded unread. Each message costs a signature check and a
/// write transaction, and a re-encoded copy of a signed message passes the deduplication
/// of iroh-gossip. Content of a discarded message arrives at the next sync of the author.
pub const GOSSIP_SENDER_BURST: f64 = 60.0;
pub const GOSSIP_SENDER_MESSAGES_PER_SECOND: f64 = 1.0;

/// Senders other than the author's devices that `GossipSenderBudgets` keeps a budget for
/// at once. Messages of further senders share one budget.
pub const MAX_BUDGETED_GOSSIP_SENDERS: usize = 16;

/// Token bucket: `GOSSIP_SENDER_BURST` tokens, refilled at
/// `GOSSIP_SENDER_MESSAGES_PER_SECOND`.
#[derive(Debug)]
struct MessageBudget {
    tokens: f64,
    refilled_at: Instant,
}

impl MessageBudget {
    fn new(now: Instant) -> Self {
        Self {
            tokens: GOSSIP_SENDER_BURST,
            refilled_at: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.refilled_at)
            .as_secs_f64();
        self.refilled_at = now;
        self.tokens =
            (self.tokens + elapsed * GOSSIP_SENDER_MESSAGES_PER_SECOND).min(GOSSIP_SENDER_BURST);
    }

    /// Take one token at `now`; false if none is left.
    fn take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// True if the bucket holds `GOSSIP_SENDER_BURST` tokens at `now`: it then equals a
    /// new bucket, so removing it changes no later decision.
    fn is_full(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens >= GOSSIP_SENDER_BURST
    }
}

/// `MessageBudget`s of one topic, per NodeId that delivered a message
/// (`iroh_gossip::Message::delivered_from`), so messages from one sender cannot use
/// up the budget of the others. The author's devices (`author_nodes`) always have their
/// own budget. Of the other senders, `MAX_BUDGETED_GOSSIP_SENDERS` have one; further
/// senders share `others`.
#[derive(Debug)]
pub struct GossipSenderBudgets {
    author_nodes: HashSet<EndpointId>,
    senders: HashMap<EndpointId, MessageBudget>,
    others: MessageBudget,
}

impl GossipSenderBudgets {
    pub fn new(author_nodes: HashSet<EndpointId>, now: Instant) -> Self {
        Self {
            author_nodes,
            senders: HashMap::new(),
            others: MessageBudget::new(now),
        }
    }

    /// Take one token of `sender`'s budget at `now`; false if none is left.
    pub fn take(&mut self, sender: EndpointId, now: Instant) -> bool {
        if let Some(budget) = self.senders.get_mut(&sender) {
            return budget.take(now);
        }
        if !self.author_nodes.contains(&sender)
            && self.budgeted_others() >= MAX_BUDGETED_GOSSIP_SENDERS
        {
            let author_nodes = &self.author_nodes;
            self.senders
                .retain(|id, budget| author_nodes.contains(id) || !budget.is_full(now));
            if self.budgeted_others() >= MAX_BUDGETED_GOSSIP_SENDERS {
                return self.others.take(now);
            }
        }
        self.senders
            .entry(sender)
            .or_insert_with(|| MessageBudget::new(now))
            .take(now)
    }

    /// Number of senders other than the author's devices with their own budget.
    fn budgeted_others(&self) -> usize {
        self.senders
            .keys()
            .filter(|id| !self.author_nodes.contains(*id))
            .count()
    }

    /// Replace the author's NodeIds. A former author NodeId keeps its budget as an
    /// ordinary sender.
    pub fn set_author_nodes(&mut self, author_nodes: HashSet<EndpointId>) {
        self.author_nodes = author_nodes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn message_budget_allows_a_burst_then_the_refill_rate() {
        let start = Instant::now();
        let mut budget = MessageBudget::new(start);
        for _ in 0..GOSSIP_SENDER_BURST as usize {
            assert!(budget.take(start));
        }
        assert!(!budget.take(start));
        // One second later one token was added, not more.
        let later = start + Duration::from_secs(1);
        assert!(budget.take(later));
        assert!(!budget.take(later));
        // The bucket does not grow above the burst size.
        let much_later = start + Duration::from_secs(3_600);
        for _ in 0..GOSSIP_SENDER_BURST as usize {
            assert!(budget.take(much_later));
        }
        assert!(!budget.take(much_later));
    }

    fn node(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn one_sender_cannot_use_up_the_budget_of_others() {
        let now = Instant::now();
        let author = node(1);
        let mut budgets = GossipSenderBudgets::new(HashSet::from([author]), now);
        let attacker = node(2);
        while budgets.take(attacker, now) {}
        assert!(budgets.take(author, now));
        assert!(budgets.take(node(3), now));
    }

    #[test]
    fn senders_beyond_the_limit_share_one_budget_and_never_displace_the_author() {
        let now = Instant::now();
        let author = node(1);
        let mut budgets = GossipSenderBudgets::new(HashSet::from([author]), now);
        // Each budgeted sender takes two tokens: its budget is full again after 2 s.
        for seed in 10..10 + MAX_BUDGETED_GOSSIP_SENDERS as u8 {
            assert!(budgets.take(node(seed), now));
            assert!(budgets.take(node(seed), now));
        }
        for _ in 0..GOSSIP_SENDER_BURST as usize {
            assert!(budgets.take(node(200), now));
        }
        assert!(!budgets.take(node(201), now));
        assert!(budgets.take(author, now));
        assert!(!budgets.senders.contains_key(&node(201)));
        // Refilled budgets hold no state; they are removed for a new sender.
        assert!(budgets.take(node(201), now + Duration::from_secs(2)));
        assert!(budgets.senders.contains_key(&node(201)));
    }
}
