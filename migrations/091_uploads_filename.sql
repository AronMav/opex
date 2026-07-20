-- 091_uploads_filename.sql
-- Store the original client-side filename on uploads rows so the serve
-- endpoint can ship it in Content-Disposition (browsers otherwise fall back
-- to the UUID path segment — user clicks "save" on a JSON upload and gets
-- '3a4632f8-...-28f42e44ec7d' instead of 'chroma_api.json').
--
-- Nullable + no default: existing rows (and any insert path that doesn't
-- know the filename — tool_output binaries, agent icons) keep working.
ALTER TABLE uploads ADD COLUMN IF NOT EXISTS filename TEXT;
