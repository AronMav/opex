-- m084: profiles — именованные наборы провайдеров/моделей/голоса/резервов.
-- Слоты: {"text":[{"provider":"...","model":"..."}], "tts":[{"provider":"...","voice":"..."}], ...}
CREATE TABLE profiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    slots JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
