-- 080_infra_decisions.sql
-- Self-healing инфраструктуры: асинхронные решения по проблемным docker-контейнерам.
-- Opex создаёт запись по итогу диагностики (pending=вопрос владельцу, done=починил,
-- dismissed=действий не требуется). UNIQUE partial index гарантирует не более одного
-- pending на контейнер. См. docs/superpowers/specs/2026-07-11-agent-infra-self-healing-design.md
CREATE TABLE IF NOT EXISTS infra_decisions (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    container         TEXT        NOT NULL,
    diagnosis         TEXT        NOT NULL,
    proposed_action   TEXT        NOT NULL DEFAULT '',
    proposed_commands JSONB       NOT NULL DEFAULT '[]'::jsonb,
    status            TEXT        NOT NULL DEFAULT 'pending',  -- pending|approved|rejected|expired|done|failed|dismissed
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at       TIMESTAMPTZ,
    resolved_by       TEXT,
    expires_at        TIMESTAMPTZ NOT NULL DEFAULT now() + interval '7 days'
);

-- Не более одного pending-решения на контейнер (enforcement на уровне БД).
CREATE UNIQUE INDEX IF NOT EXISTS idx_infra_decisions_one_pending
    ON infra_decisions (container) WHERE status = 'pending';

-- Дебаунс-запросы: недавние записи по контейнеру.
CREATE INDEX IF NOT EXISTS idx_infra_decisions_container_created
    ON infra_decisions (container, created_at DESC);
