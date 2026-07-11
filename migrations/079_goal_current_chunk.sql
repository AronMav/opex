-- Stage C batch B: current chunk index for plan-decompose-react on initiative goals.
ALTER TABLE session_goals ADD COLUMN IF NOT EXISTS current_chunk INT NOT NULL DEFAULT 0;
