-- Web search capability: provider_active becomes multi-row per capability with priority.
-- Composite-PK columns are NOT NULL, so NULL placeholder rows ("no active provider")
-- are dropped first and the FK on_delete is switched from SET NULL to CASCADE.

DELETE FROM provider_active WHERE provider_name IS NULL;

ALTER TABLE provider_active ADD COLUMN priority INT NOT NULL DEFAULT 100;
ALTER TABLE provider_active ALTER COLUMN provider_name SET NOT NULL;

ALTER TABLE provider_active DROP CONSTRAINT provider_active_pkey;
ALTER TABLE provider_active ADD PRIMARY KEY (capability, provider_name);

ALTER TABLE provider_active DROP CONSTRAINT provider_active_provider_name_fkey;
ALTER TABLE provider_active
  ADD CONSTRAINT provider_active_provider_name_fkey
  FOREIGN KEY (provider_name) REFERENCES providers(name)
  ON DELETE CASCADE ON UPDATE CASCADE;
