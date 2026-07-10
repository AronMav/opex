# Runbook: карантин отравленной сессии (soul)

Когда: подозрение, что враждебный контент сессии `{SID}` пророс в биографию/SELF.md.

## 1. Найти и удалить события сессии (сохранить id для шага 2)

```sql
-- посмотреть перед удалением
SELECT id, content FROM memory_chunks WHERE source = 'soul_event:{SID}';
-- удалить, вернув id
DELETE FROM memory_chunks WHERE source = 'soul_event:{SID}' RETURNING id;
```

## 2. Транзитивно удалить производные рефлексии (lineage-пересечение до фиксированной точки)

```sql
WITH RECURSIVE tainted AS (
    -- семя: id, удалённые на шаге 1 (подставить список)
    SELECT unnest(ARRAY[...]::uuid[]) AS id
  UNION
    -- рефлексии, чей lineage пересекается с уже заражёнными id
    SELECT mc.id
    FROM memory_chunks mc
    JOIN tainted t ON mc.lineage @> ARRAY[t.id]
    WHERE mc.kind = 'reflection'
)
DELETE FROM memory_chunks
WHERE id IN (SELECT id FROM tainted)
  AND kind = 'reflection'
RETURNING id, content;
```

## 3. Откатить SELF.md

Через CheckpointPanel в чате агента (или API checkpoint-restore) — выбрать
снапшот ДО первого заражённого цикла рефлексии (время из шага 2 RETURNING /
audit_log tool_name='soul_reflection').

## 4. Проверка

`SELECT count(*) FROM memory_chunks WHERE source = 'soul_event:{SID}'` → 0;
рендер SELF.md в context-breakdown не содержит заражённых буллетов.
