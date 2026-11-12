-- Signed deletions of interactions. An interaction with a stored deletion is not
-- inserted again.
CREATE TABLE interaction_tombstones (
    author TEXT NOT NULL REFERENCES registrations(pubkey),
    id TEXT NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (author, id)
) WITHOUT ROWID;
