-- Each device has a random transport key. NodeIds derived from a master key are no
-- longer used, so stored device lists of the local user and of peers are removed.
DELETE FROM linked_devices;
ALTER TABLE linked_devices DROP COLUMN is_primary;
DELETE FROM peer_transports;
DELETE FROM peer_device_announcements;
