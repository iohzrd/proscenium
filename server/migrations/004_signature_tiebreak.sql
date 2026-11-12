-- Signatures of the stored profile and device list. Between two profiles with the same
-- `updated_at`, or two device lists with the same version, the one with the greater
-- signature string is stored, so the stored row does not depend on arrival order. NULL
-- while none is stored.
ALTER TABLE registrations ADD COLUMN profile_signature TEXT;
ALTER TABLE registrations ADD COLUMN announcement_signature TEXT;
