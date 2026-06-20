CREATE TABLE channel_voice_modes (
    channel    TEXT NOT NULL,
    chat_id    TEXT NOT NULL,
    mode       TEXT NOT NULL DEFAULT 'off'
               CHECK (mode IN ('off', 'on')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel, chat_id)
);
