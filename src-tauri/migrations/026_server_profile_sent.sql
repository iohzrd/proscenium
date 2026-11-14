-- Local time in milliseconds of the last own profile a server accepted. A registered
-- server not sent the profile within `SERVER_PROFILE_REFRESH` receives it again, so the
-- server does not expire the registration for inactivity.
ALTER TABLE servers ADD COLUMN profile_sent_at INTEGER NOT NULL DEFAULT 0;
