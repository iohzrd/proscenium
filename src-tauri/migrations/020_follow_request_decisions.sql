-- Time of the approval or denial of a follow request (0 while pending). Device sync
-- keeps the later decision.
ALTER TABLE follow_requests ADD COLUMN decided_at INTEGER NOT NULL DEFAULT 0;
UPDATE follow_requests SET decided_at = created_at WHERE status != 'pending';
