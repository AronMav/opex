# Self-healing инфраструктуры: Watchdog-триггер → Opex

**Дата:** 2026-07-11
**Статус:** дизайн одобрен, ожидает плана реализации
**Триггер:** контейнер `docker-tts-silero-1` застрял в состоянии `Created` и
триггерил мониторинг Watchdog; Silero давно снят с эксплуатации (активный TTS =
`qwen3-tts-local`), но осиротевший контейнер никто не убирал. Watchdog **алертит**
про такие проблемы, но **не чинит** — эта дыра и проявилась.

## Цель

Дать base-агенту Opex способность **сам разбирать инфраструктурные проблемы
docker-контейнеров**: безопасное чинить автоматически, при сомнении — задавать
владельцу вопрос с кнопками да/нет и выполнять решение по ответу.

## Не-цели (v1)

- Native managed-процессы (toolgate/channels) — их уже авто-рестартит
  `process_manager` при краше.
- Диск / память / зомби-процессы / прочая широкая инфра — будущая итерация.
- Авто-коммит правок серверного `docker-compose.yml` в git (в v1 — только
  уведомление владельцу о git-дрейфе).

## Ключевые архитектурные решения (из брейншторма)

1. **Автономия = «чинит безопасное, при сомнении спрашивает»** (не молчаливый
   алерт постфактум — именно вопрос перед рискованным действием).
2. **Механизм вопроса = структурированное подтверждение** (Telegram inline-кнопки
   да/нет), а не свободный текст.
3. **Триггер = Watchdog-событие** (не heartbeat-polling): Watchdog уже сканирует
   контейнеры каждый цикл — пусть он и запускает Opex с готовым диагнозом.
   Событийно, реакция за минуты, без дублирования сканирования.
4. **Исполнитель одобренного = re-trigger Opex** (не детерминированный
   core-executor): реальный кейс (silero) требует И `docker rm`, И правки
   `compose` — иначе контейнер вернётся при следующем `compose up`. Это ровно то,
   что Opex через `code_exec` уже умеет; детерминированный executor правку файла
   не сделает.

## Существующая инфраструктура, на которую опираемся

- **Watchdog `checker::check_docker_containers`**
  ([crates/opex-watchdog/src/checker.rs:43](../../../crates/opex-watchdog/src/checker.rs#L43)) —
  уже гоняет `docker ps -a` каждый цикл, возвращает `ContainerInfo{healthy}` где
  `healthy = status.starts_with("Up")`. `main.rs` уже держит
  `was_container_unhealthy: HashMap` для дебаунса алертов.
- **`handle_isolated_via_pipeline`**
  ([crates/opex-core/src/scheduler/mod.rs:1525](../../../crates/opex-core/src/scheduler/mod.rs#L1525)) —
  RPC-style запуск агентской сессии из cron/heartbeat, возвращает финальный текст.
- **Telegram inline-callback intercept**
  ([crates/opex-core/src/gateway/handlers/channel_ws/inline.rs:160](../../../crates/opex-core/src/gateway/handlers/channel_ws/inline.rs#L160)) —
  готовый паттерн префиксов `approve:UUID` / `reject:UUID` / `clarify:` / `hm:`,
  owner-only проверка, стор контекста для ≤64-байт callback_data.
- **`notifications::notify`**
  ([crates/opex-core/src/gateway/handlers/notifications.rs:148](../../../crates/opex-core/src/gateway/handlers/notifications.rs#L148)) —
  запись в БД + WS-broadcast (колокольчик UI).
- **base-агент `code_exec` на хосте** (без sandbox) — может вызывать `docker` CLI
  напрямую и править файлы в service-dirs.

`pending_approvals` (m001) **не** переиспользуем: там `session_id` FK +
`tool_name`, флоу синхронный (waiter с таймаутом). Наш флоу асинхронный (владелец
отвечает через часы) → отдельная лёгкая таблица.

## Поток (end-to-end)

```text
Watchdog (каждый цикл)
  └─ docker ps -a → классификация каждого контейнера
       healthy(Up*) → игнор
       transient(Created/Restarting/недавний Exited, < 2 циклов) → игнор
       needs_attention(то же состояние ≥ 2 циклов подряд) → кандидат
         └─ POST /api/internal/infra-event {docker_name, status, kind}
              └─ [core] дебаунс: есть pending decision ИЛИ done/dismissed/rejected < 24ч? → no-op
                   └─ spawn base_engine.handle_isolated_via_pipeline(диагноз-затравка)   [fire-and-forget, 202]
                        ├─ Opex диагностирует (docker inspect, сверка с compose/провайдерами/портами)
                        ├─ SAFE (упавший compose-сервис) → docker restart сам → decision status=done
                        ├─ НЕ ТРЕБУЕТСЯ (штатно) → decision status=dismissed + причина
                        └─ СОМНЕНИЕ → POST /api/infra/decisions {diagnosis, proposed_action, proposed_commands} (pending)
                             └─ [core] notify() UI-кнопки да/нет  (+ Telegram infra:ok/no:UUID — stretch)
                                  └─ владелец подтверждает (UI /resolve или Telegram; owner-only)
                                       └─ resolve_infra_decision: approved → spawn Opex «выполни решение UUID»
                                            └─ Opex code_exec: rm + правка compose → PATCH /api/infra/decisions/{id} done|failed
```

## Компоненты

### 1. Детекция (Watchdog, расширение `checker.rs` + `main.rs`)

Классификация контейнера в `check_docker_containers` (или рядом):

| Класс | Условие | Действие |
| --- | --- | --- |
| `healthy` | `status` начинается с `Up` | игнор |
| `transient` | `Created` / `Restarting` / недавний `Exited`, замечен **< 2 циклов** | игнор (возможен деплой в процессе) |
| `needs_attention` | то же нездоровое состояние держится **≥ 2 циклов подряд** | кандидат на триггер |

- Grace-дебаунс реализуется расширением существующего `was_container_unhealthy`
  (счётчик последовательных циклов вместо булева, либо параллельный
  `unhealthy_streak: HashMap<String, u32>`).
- **Исключения** (никогда не кандидат): имя содержит `postgres`; имя начинается с
  `mcp-` (эфемерные on-demand). Совпадает с уже существующими `continue`-скипами
  в `checker.rs`.
- Триггер шлётся через новый HTTP-вызов из watchdog в core (аналогично тому, как
  `alerter::send` уже постит в `/api/channels/notify`).

### 2. Мост Watchdog → Core (новый endpoint)

`POST /api/internal/infra-event` (loopback, Bearer-auth), body:
```json
{ "docker_name": "docker-tts-silero-1", "status": "Created", "kind": "needs_attention" }
```
Core:
1. **Дебаунс:** если для этого `docker_name` уже есть **любая** недавняя строка в
   `infra_decisions` — `pending` (вопрос висит), либо `done`/`dismissed`/`rejected`
   за последние **24 ч** (уже разобрано) → `200 {"skipped": true}`. Не плодим
   сессии/уведомления и не жжём LLM на уже-разобранном контейнере (см.
   safety-инвариант 7).
2. Иначе — `tokio::spawn` → `engine.handle_isolated_via_pipeline(seed_msg)`,
   вернуть `202`. Результат сессии не ожидается.

**Как handler получает engine base-агента** (проверено против кода):
`AppState.agents: AgentCore` уже даёт `get_engines_map()` и `first_engine()`
([crates/opex-core/src/gateway/clusters/agent_core.rs:48](../../../crates/opex-core/src/gateway/clusters/agent_core.rs#L48)),
а `handle.engine.cfg().agent.base` — флаг base. Handler берёт map, выбирает первый
engine с `cfg().agent.base == true`, вызывает на нём `handle_isolated_via_pipeline`
(ровно так, как scheduler вызывает `engine2.handle_isolated_via_pipeline` для cron).

**Агент-респондер:** первый агент с `base = true` (у пользователя — Opex).
Хардкод имени не делаем.

**Диагноз-затравка (`seed_msg`):** кратко — что обнаружено + указание подтянуть
скилл `infra-triage`. Пример:
> `[Infra] Watchdog обнаружил проблемный контейнер `docker-tts-silero-1` в`
> `состоянии `Created` (держится ≥2 циклов). Используй скилл infra-triage:`
> `продиагностируй и, если безопасно — почини сам; иначе создай infra-решение`
> `с вопросом владельцу.`

### 3. Реакция Opex (новый base-скилл `config/skills/infra-triage.md`)

System-скилл, доступен только base-агентам. Содержит протокол:

- **Диагностика:** `docker inspect <name>`; сверка с активным
  `~/opex/docker/docker-compose.yml` (есть ли сервис, закомментирован ли);
  сверка с активными провайдерами (`GET /api/providers` — используется ли сервис);
  проверка порта (`ss -ltnp`). Ровно шаги ручного разбора кейса silero.
- **SAFE → чинит сам, без вопроса:** контейнер, который *должен* работать и просто
  упал — известный compose-сервис в `Exited`/`Restarting` → `docker restart <name>`.
  Идемпотентно, обратимо.
- **СОМНЕНИЕ → спрашивает:** любое удаление контейнера (`docker rm`), любая правка
  `compose`, любой незнакомый контейнер → `POST /api/infra/decisions` с
  `diagnosis`, `proposed_action` (человекочитаемо) и `proposed_commands`
  (зафиксированные шаги для дословной передачи при исполнении).
- **Ничего не требуется** (напр. контейнер штатно остановлен, ложная тревога) →
  Opex создаёт запись со `status='dismissed'` + причина. Это подавляет повторные
  триггеры на 24 ч (safety-инвариант 7) — иначе Opex дёргался бы каждый цикл.
- **Отчёт:** по завершении Opex резюмирует — что починил / какой вопрос задал /
  почему отклонил.

**Итог для каждого триггера обязателен:** Opex всегда оставляет ровно одну запись в
`infra_decisions` (`pending` | `done` | `dismissed`) — «молчаливого» завершения без
следа быть не должно, иначе анти-петлевой дебаунс не сработает.

### 4. Ask-flow (новая таблица + API + кнопки)

**Миграция — таблица `infra_decisions`:**
```sql
CREATE TABLE infra_decisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    container TEXT NOT NULL,
    diagnosis TEXT NOT NULL,
    proposed_action TEXT NOT NULL,
    proposed_commands JSONB NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'pending',  -- pending|approved|rejected|expired|done|failed|dismissed
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by TEXT,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '7 days'
);
-- UNIQUE partial index: БД гарантирует не более одного pending-решения на контейнер.
-- Вставка второго pending для того же контейнера упадёт → core ловит и трактует как
-- «уже есть pending» (совпадает с core-дебаунсом).
CREATE UNIQUE INDEX idx_infra_decisions_one_pending ON infra_decisions (container) WHERE status = 'pending';
```

**⚠️ Доставка подтверждения — критический нюанс (проверено против кода).**
Opex создаёт decision из **изолированной** сессии (`handle_isolated_via_pipeline`),
у которой **нет `chat_id`** активного Telegram-чата. Существующий approval-flow
([approval_manager.rs:78](../../../crates/opex-core/src/agent/approval_manager.rs#L78))
шлёт Telegram-кнопки через `ChannelAction`, извлекая адресата из `context` активной
сессии — в изолированном контексте этого `context` нет. Поэтому:

- **Основной путь (v1) = UI-notification** (`notify()` + WS-broadcast). Он
  **не зависит от `chat_id`** и работает из любого контекста, включая изолированный.
  Требует доработки UI: notification типа `infra_decision` рендерит **actionable
  кнопки да/нет** (сейчас notification только кликабельна→навигация — см. CLAUDE.md);
  нажатие вызывает `POST /api/infra/decisions/{id}/resolve {approved: bool}`.
- **Дополнительный путь (Telegram-кнопки)** — требует **явного резолва DM-канала
  владельца** base-агента (`owner_id` → `agent_channels` → `chat_id`) и отправки
  `ChannelAction` с `target_channel` + сконструированным `context{chat_id}`, либо
  расширения `/api/channels/notify` полем `buttons`. Помечено как **stretch v1**:
  если резолв канала доступен — дублируем кнопки в Telegram; если нет — UI-путь
  самодостаточен.

**API:**
- `POST /api/infra/decisions` (агентский, base-only) — создать. Core после вставки:
  `notify(type="infra_decision", data={decision_id, container, proposed_action})` →
  UI-колокольчик с actionable-кнопками. Опционально (stretch) — Telegram-кнопки
  `infra:ok:UUID` / `infra:no:UUID` (UUID = 36 символов, влезает в 64-байт
  callback_data — стор контекста не нужен) при доступном DM-канале владельца.
- `POST /api/infra/decisions/{id}/resolve {approved: bool}` (**owner-guarded** —
  сессия/токен владельца) — путь подтверждения из UI. Единая точка с Telegram-callback
  (см. Компонент 5).
- `PATCH /api/infra/decisions/{id}` — обновить статус (`done` / `failed`), вызывает
  Opex по завершении исполнения.
- `GET /api/infra/decisions` — список (для UI).

**TTL:** фоновая задача (или проверка при каждом Watchdog-триггере) помечает
`pending` строки с `expires_at < now()` как `expired` — вопрос не висит вечно.

### 5. Исполнение одобренного (единая resolve-функция, два входа)

Оба входа — UI и Telegram — сводятся к одной core-функции
`resolve_infra_decision(id, approved, resolved_by)`:

- **Вход A — UI:** `POST /api/infra/decisions/{id}/resolve {approved}` (owner-guarded
  middleware'ом, как прочие агентские API).
- **Вход B — Telegram (stretch):** новый префикс в callback-intercept
  ([inline.rs:160](../../../crates/opex-core/src/gateway/handlers/channel_ws/inline.rs#L160)),
  `infra:ok:UUID` / `infra:no:UUID`, **owner-only** (существующая проверка отправителя
  в inline.rs). Парсит UUID, вызывает ту же `resolve_infra_decision`.

Логика `resolve_infra_decision`:
- **Идемпотентность:** если `status != pending` → no-op + ответ «решение уже
  обработано» (защита от гонки UI/Telegram/двойного клика).
- `approved == false` → `status=rejected`, `resolved_by`, ответ «отклонено».
- `approved == true` → `status=approved` → `tokio::spawn`
  `base_engine.handle_isolated_via_pipeline(...)` с сообщением:
  > `[Infra] Владелец одобрил решение {UUID}: {proposed_action}. Выполни`
  > `зафиксированные шаги: {proposed_commands}. По завершении вызови`
  > `PATCH /api/infra/decisions/{UUID} со статусом done или failed и сообщи итог.`
  Opex через `code_exec` выполняет (rm + правка серверного compose), затем PATCH.

### 6. Safety-инварианты

1. **Grace-дебаунс** ≥2 цикла watchdog **+** core-дебаунс 30 мин → не реагируем на
   transient-состояния и деплой в процессе.
2. **Один `pending` decision на контейнер** — гарантируется UNIQUE partial index
   на уровне БД (не только проверкой в core); гонка вставок разрешается корректно.
3. **Рискованное только после явного «да»** владельца; предложение зафиксировано в
   `proposed_commands` дословно.
4. **Никогда без вопроса:** `postgres` и любой контейнер с данными (исключён на
   этапе детекции).
5. **Git-дрейф:** правка серверного `docker-compose.yml` не синкается в git
   (deploy-скрипт свопит только Rust-бинарники — см. `reference-deploy-gaps`).
   После правки Opex **явно уведомляет** владельца в отчёте: «серверный compose
   изменён — обнови git-версию». В v1 — уведомление, не авто-коммит.
6. **owner-only на обоих путях подтверждения:** UI `/resolve` — auth-middleware
   (Bearer владельца); Telegram-callback — существующая проверка отправителя в
   `inline.rs`. Идемпотентность `resolve_infra_decision` защищает от гонки двух путей.
7. **Анти-петля / контроль стоимости (критично):** каждый триггер = LLM-сессия
   Opex (деньги). Одного core-дебаунса 30 мин мало: контейнер, который вечно в
   проблемном состоянии, а Opex решил *не* действовать (не починил и не создал
   decision), иначе триггерился бы каждые 30 мин бесконечно. Поэтому Opex по итогу
   диагностики **всегда** оставляет отметку в `infra_decisions`: либо `pending`
   (задал вопрос), либо `done` (починил сам), либо **`dismissed`** (проверено,
   действий не требуется — напр. контейнер штатно остановлен) с причиной.
   Core-дебаунс смотрит на **любую** недавнюю запись для контейнера, а не только
   `pending`: наличие `dismissed`/`done` за последние N часов (дефолт 24 ч)
   подавляет повторный триггер. Состояние живёт в БД, поэтому переживает рестарт
   watchdog (streak-счётчик в памяти watchdog — лишь grace-фильтр, не дедуп).

## Дефолты (конфигурируемо, но не в UI v1)

- Агент-респондер = первый `base = true`.
- Grace = 2 цикла watchdog.
- Core-дебаунс на **разобранный** контейнер (`done`/`dismissed`/`rejected`) = 24 ч.
- TTL `pending` decision = 7 дней (→ `expired`).
- Safe-набор v1 = **только** перезапуск упавшего compose-сервиса. Удаление и
  правка compose — всегда через вопрос.

## Затрагиваемые файлы (ориентир для плана)

- `crates/opex-watchdog/src/checker.rs` — классификация контейнеров.
- `crates/opex-watchdog/src/main.rs` — streak-дебаунс + вызов infra-event.
- `crates/opex-watchdog/src/` (новый или в `alerter.rs`) — HTTP-клиент infra-event.
- `crates/opex-core/src/gateway/handlers/` (новый `infra.rs`) — `/api/internal/infra-event`,
  `/api/infra/decisions` CRUD + `/resolve`; общая `resolve_infra_decision`.
- `crates/opex-core/src/db/` (новый `infra_decisions.rs`) — запросы.
- `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` — префикс `infra:`
  (stretch, Telegram-путь).
- `migrations/mNNN_infra_decisions.sql` — таблица.
- `config/skills/infra-triage.md` — протокол диагностики (новый base-скилл).
- **UI:** notification-компонент — actionable кнопки да/нет для типа `infra_decision`
  с обработчиком, вызывающим `POST /api/infra/decisions/{id}/resolve`. Опционально
  страница/список pending-решений.
- Проводка spawn-триггера (переиспользовать путь cron/heartbeat к
  `engine.handle_isolated_via_pipeline`; engine base-агента — из
  `AgentCore::get_engines_map()` + фильтр `cfg().agent.base`).

## Открытые вопросы для этапа плана

- **TTL-expiry:** отдельный фоновый tick или ленивая проверка при Watchdog-триггере
  (склоняемся к ленивой — меньше кода).
- **Telegram-путь (stretch):** резолв DM-канала владельца из `owner_id` →
  `agent_channels` → `chat_id`, и способ отправки inline-кнопок вне активной сессии
  (расширить `/api/channels/notify` полем `buttons` vs `ChannelAction` с явным
  `target_channel`+`context{chat_id}`). Если резолв нетривиален — Telegram-путь
  переносится за v1, UI-путь остаётся самодостаточным.
- **owner-guard для `/resolve`:** переиспользовать существующий auth-middleware
  (Bearer владельца) — подтвердить, что UI шлёт токен, а не полагается на сессию.
