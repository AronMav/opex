-- File Handler Hub Phase 3: provenance tag for file-derived messages.
-- NULL = ordinary message (trunk default). 'file_handler' = produced by a
-- handler run. The <file_output> provenance wrapper is baked into the stored
-- `content` at persist time (no read-path edit needed); this column lets the
-- UI strip the wrapper for display in a later follow-up.
ALTER TABLE messages ADD COLUMN source TEXT;
