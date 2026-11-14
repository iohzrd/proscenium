-- Server time in milliseconds of the last accepted signed request or completed sync of
-- the registration. An active registration without either for
-- `limits.registration_expiry_days` is deactivated.
ALTER TABLE registrations ADD COLUMN last_active_at INTEGER NOT NULL DEFAULT 0;
-- 1 if the registration was deactivated for inactivity: a profile update for it is then
-- answered 410, so the client registers again. 0 otherwise.
ALTER TABLE registrations ADD COLUMN expired INTEGER NOT NULL DEFAULT 0;
UPDATE registrations SET last_active_at = MAX(registered_at, last_seen);
