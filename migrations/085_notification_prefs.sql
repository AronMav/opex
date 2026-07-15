-- m085: notification_prefs — глобальные пер-типовые настройки уведомлений
-- (single-operator): mute + sound. Без CHECK на notification_type — набор типов
-- открытый (любая строка, с которой зовут notify()), CHECK пришлось бы расширять
-- на каждый новый тип.
CREATE TABLE IF NOT EXISTS notification_prefs (
    notification_type TEXT        PRIMARY KEY,
    muted             BOOLEAN     NOT NULL DEFAULT FALSE,
    sound             BOOLEAN     NOT NULL DEFAULT TRUE,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
