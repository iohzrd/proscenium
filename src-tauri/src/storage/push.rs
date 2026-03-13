// Push outbox storage has been removed. Push is now fire-and-forget via gossip::attempt_push.
// The push_outbox table migration (010_push_outbox.sql) is retained for schema history.
use super::Storage;

impl Storage {}
