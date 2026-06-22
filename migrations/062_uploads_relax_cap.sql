-- Relax the uploads.size_bytes CHECK from 20 MB (052) to 50 MB. Migration 052
-- is immutable; this is the forward path. See spec §4.5. The named auto-CHECK
-- constraint from 052 is dropped and re-added with the higher ceiling.

ALTER TABLE uploads DROP CONSTRAINT IF EXISTS uploads_size_bytes_check;
ALTER TABLE uploads ADD CONSTRAINT uploads_size_bytes_check
    CHECK (size_bytes >= 0 AND size_bytes <= 52428800);

COMMENT ON CONSTRAINT uploads_size_bytes_check ON uploads IS
    '50 MB per-file ceiling (relaxed from 052''s 20 MB). Runtime SoT is [uploads] max_upload_bytes; this CHECK is the DB backstop.';
