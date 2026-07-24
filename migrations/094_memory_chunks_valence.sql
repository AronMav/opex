-- Feature #5 (mood → retrieval salience): per-chunk emotional valence so soul
-- retrieval can bias by mood-congruence (a sad agent better remembers negative
-- events). Nullable: legacy rows, facts and reflections are NULL = neutral and
-- receive no congruence bias. Populated at write-time for kind='event' chunks
-- from the session's appraised valence (knowledge_extractor save_events).
ALTER TABLE memory_chunks ADD COLUMN IF NOT EXISTS valence REAL;
