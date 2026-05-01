-- pg_trgm: trigram similarity для multilingual поиска (русский/CJK/опечатки).
-- Дополняет существующий FTS — не заменяет.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- gin_trgm_ops оптимизирует операторы %, <%, <<% (НЕ similarity() > X).
-- В Rust-коде используем оператор `%` с set_limit() per-session.
CREATE INDEX IF NOT EXISTS idx_memory_chunks_content_trgm
  ON memory_chunks USING gin (content gin_trgm_ops);

COMMENT ON INDEX idx_memory_chunks_content_trgm IS
  'Trigram GIN index for fuzzy/multilingual content search via memory_queries.search_trigram.';
