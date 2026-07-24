-- Audit P2-4: the Rust layer clamps mood valence to [-1, 1], but nothing
-- stopped a direct SQL write from persisting an out-of-range value (e.g. 99).
-- Add a CHECK constraint as a defence-in-depth boundary. Existing rows are
-- clamped first so the migration cannot fail on legacy out-of-range data.
UPDATE agent_emotion_state SET valence = -1 WHERE valence < -1;
UPDATE agent_emotion_state SET valence = 1 WHERE valence > 1;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'agent_emotion_state_valence_range'
    ) THEN
        ALTER TABLE agent_emotion_state
            ADD CONSTRAINT agent_emotion_state_valence_range
            CHECK (valence BETWEEN -1 AND 1);
    END IF;
END $$;
