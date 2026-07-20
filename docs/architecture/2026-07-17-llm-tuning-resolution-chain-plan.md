# LLM Tuning Resolution Chain (max_tokens / temperature) — Plan

**Дата:** 2026-07-17
**Статус:** proposed
**Триггер:** обсуждение поля `[agent] max_tokens`. Претензия владельца: непонятно, зачем
ручное поле, если лимит есть у модели; логично, что желаемая длина/стиль ответа зависят от
**поверхности** (канал), а потолок — от **провайдера/модели**.

## 1. Проблема

Сейчас `max_tokens` и `temperature` — плоские поля на агенте
([config/mod.rs:1000-1003](../../crates/opex-core/src/config/mod.rs#L1000)). Два дефекта:

1. **`max_tokens` пустой → тихий рез.** Для Anthropic параметр обязателен; при `None`
   берётся **хардкод 8192** ([anthropic/request.rs:78](../../crates/opex-core/src/agent/providers/anthropic/request.rs#L78)),
   а не реальный output-лимит модели. Anthropic-агент без явного значения молча режется на 8k,
   хотя модель тянет больше. (OpenAI при `None` параметр опускает → провайдер сам берёт максимум,
   [openai/request.rs:61](../../crates/opex-core/src/agent/providers/openai/request.rs#L61).)
2. **Нет пер-канального контроля.** Желаемая многословность зависит от поверхности
   (Telegram/голос → коротко, web → длинно), но задать это по каналу нельзя.

## 2. Дизайн: цепочка разрешения

Заменить «плоское поле» многоуровневым резолвом с явным приоритетом.

### 2.1. max_tokens

```
effective_max_tokens =
      channel override        (agent_channels.config.max_tokens)   ← краткость по поверхности
   ?? agent max_tokens         (config, опциональный кап)
   ?? catalog output limit     (opex_catalog::global_output)        ← потолок модели, авто
   ?? 8192                      (fallback, если каталог не знает модель)
   → clamp по catalog output limit (уже делается для OpenAI, [openai/request.rs:64](../../crates/opex-core/src/agent/providers/openai/request.rs#L64))
```

Эффект: пусто везде = **максимум модели** (уходит рез Anthropic на 8k); поле агента/канала
остаётся опциональным «капом ниже модельного». Каталожный output-лимит — это и есть «на
конкретного провайдера» без ручного knob на каждом провайдере.

### 2.2. temperature

Та же логика, **но короче** (temperature — не свойство размера модели):

```
effective_temperature =
      channel override   (agent_channels.config.temperature)
   ?? route override      (ProviderRouteConfig.temperature — УЖЕ есть, [config/mod.rs:934](../../crates/opex-core/src/config/mod.rs#L934))
   ?? agent temperature   (config, default 0.7)
```

- **Нет каталожного потолка.**
- **Сохранить гейт `allow_temperature`** — часть моделей не принимают параметр
  ([openai/request.rs:58](../../crates/opex-core/src/agent/providers/openai/request.rs#L58)); если модель
  не поддерживает — параметр не шлём независимо от резолва.
- Пер-роутовый override уже существует — добавляем только канальный слой сверху.

### 2.3. Хранение пер-канального override

`agent_channels.config` (JSONB) уже несёт пер-канальные настройки — кладём туда опциональные
`max_tokens` / `temperature`. Это НЕ креденшелы (не редактируются как секреты). Резолв
выполняется при сборке тела LLM-запроса, где известен канал текущего запроса.

## 3. Ревизия остальных полей (что НЕ трогаем и почему)

Проверены все поля `AgentSettings`. Под вынос/резолв подходит только sampling/output-тюнинг.

**A. В цепочку (этот план):** `max_tokens`, `temperature`.

**A-опц. (отложено, слабый кейс):** `max_history_messages` («глубина памяти по поверхности»),
`max_tools_in_context` (скорее свойство модели). `language` — **решено НЕ переносить**
(часть персоны агента, остаётся плоским полем).

**B. Провайдерский слой — УЖЕ на своём месте, дублировать на агенте/канале НЕ надо:**
`provider`/`model` (deprecated m084 → Профиль), `provider_connection`, `fallback_provider`,
`tts_provider`, `imagegen_provider`, `max_failover_attempts`, `routing`, и `prompt_cache`
(уже резолвится `agent → provider options → false`, [factory.rs](../../crates/opex-core/src/agent/providers/factory.rs) —
готовый образец слоистого резолва). Inline `max_tokens` на роут/провайдер был удалён намеренно
(spec §4.7, [config/mod.rs:909](../../crates/opex-core/src/config/mod.rs#L909)) — не возвращаем knob на каждый
провайдер; если override на модель нужен — один раз на Профиле.

**C. Идентичность/governance — жёстко агентские, per-channel НЕ выносим** (иначе запутанная
поверхность атаки и невозможность аудита): `access`, `soul`, `drift`, `initiative`, `emotion`,
`heartbeat`, `tools`, `approval`, `hooks`, `tool_loop`, `watchdog`, `skill_review`, `session`,
`delegation`, `daily_budget_tokens`, `tool_dispatcher`, `base`, `name`, `profile`, `language`.

**Принцип:** пер-канальный override оправдан только там, где поверхность реально меняет
желаемое поведение (многословность, стиль). Governance/tool-policy по каналам не размазываем.

## 4. Порядок работ

1. Хелпер резолва `resolve_max_tokens(channel_cfg, agent_cfg, provider, model)` и
   `resolve_temperature(...)` — единая точка, вызывается при сборке запроса.
2. Anthropic: заменить `unwrap_or(8_192)` на резолв (catalog output limit, затем 8192-fallback).
   OpenAI: `None` → авто-каталог вместо «опустить» (или оставить опускание — эквивалентно
   максимуму; но резолв нужен для канального override и бюджета thinking).
3. Пер-канальный слой: читать `max_tokens`/`temperature` из `agent_channels.config`; прокинуть
   канал текущего запроса в точку резолва.
4. UI: подпись поля агента → «пусто = максимум модели»; в настройках канала — опциональные
   `max_tokens`/`temperature` override.
5. Тесты (§5).

## 5. Тесты

- max_tokens: пусто → catalog output limit; агент задан → его значение (clamp по потолку);
  канал задан → перекрывает агента; каталог не знает модель → 8192.
- Anthropic без агентского max_tokens больше НЕ режется на 8k, если каталог знает модель.
- temperature: канал > route > agent; модель без поддержки → параметр не отправлен
  (`allow_temperature=false`) независимо от резолва.
- backward-compat: агент со старым `max_tokens = 16384` и без канального override ведёт себя
  как раньше.

## 6. Скоуп / отложено

- В скоупе v1: резолв-цепочка + пер-канальный override для `max_tokens` и `temperature`.
- Отложено: `max_history_messages`/`max_tools_in_context` пер-канально; thinking-budget
  как отдельный резолв (пока следует за max_tokens).
- Не делаем: `language` пер-канально; knob `max_tokens` на каждый провайдер/роут.
