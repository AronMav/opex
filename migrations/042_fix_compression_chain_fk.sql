-- Fix: add ON DELETE SET NULL to parent_session_id FK so parent sessions
-- can be deleted without FK violations (child orphaned, parent_session_id set to NULL).
ALTER TABLE sessions DROP CONSTRAINT IF EXISTS sessions_parent_session_id_fkey;
ALTER TABLE sessions ADD CONSTRAINT sessions_parent_session_id_fkey
  FOREIGN KEY (parent_session_id) REFERENCES sessions(id) ON DELETE SET NULL;
