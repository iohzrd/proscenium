-- Add signature column to profiles for signed profile updates.
ALTER TABLE profiles ADD COLUMN signature TEXT NOT NULL DEFAULT '';
