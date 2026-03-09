CREATE TABLE IF NOT EXISTS profiles (
    pubkey TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    bio TEXT NOT NULL,
    avatar_hash TEXT,
    avatar_ticket TEXT,
    visibility TEXT NOT NULL DEFAULT 'public',
    signature TEXT NOT NULL DEFAULT ''
);
