# Спека №2: Голосовой UX web-чата — чистый транскрипт, очередь, стриминговая озвучка

**Дата:** 2026-07-15
**Статус:** дизайн согласован (брейншторм 2026-07-14/15)
**Зависимость:** [2026-07-15-profiles-design.md](2026-07-15-profiles-design.md) (спека №1) — резолюция TTS-провайдера и голоса идёт через профиль агента. Реализуется ПОСЛЕ неё.

## 1. Контекст: разбор сессии `b79fe72c` (2026-07-14, Arty, web UI)

Аудит последней голосовой сессии выявил четыре дефекта:

1. **`[MM:SS]`-тайм-коды в голосовом вводе.** [stt_openai.py:40-51](../../../toolgate/providers/stt_openai.py) всегда запрашивает `verbose_json` и склеивает сегменты в `[00:01] текст…` (добавлено для видео-конспектов, коммит 4ccbf9df), а [/transcribe](../../../toolgate/routers/stt.py) отдаёт как есть — мусор в сообщении пользователя.
2. **Голосовая автоотправка оборвала идущий ход** — `sendMessage` при стриминге делает interrupt-and-send ([stream-control.ts:39-44](../../../ui/src/stores/chat/actions/stream-control.ts)); два tool call'а остались `[interrupted:verify]`, работа агента потеряна.
3. **Озвучен отладочный мусор.** Финальное сообщение ассистента — literal `(Empty response: {'content': [{'type': 'thinking', …` (апстрим-прокси сериализует thinking-only ответ в текст). Из-за пер-итерационной сегментации live-сообщений ([stream-processor.ts:318-339](../../../ui/src/stores/stream/stream-processor.ts)) аудио-part от `synthesize_speech` лежит в предыдущем сообщении, `lastAssistantAudioUrl` смотрит только последнее → фоллбек на текст → TTS зачитал Python-дамп вслух.
4. **Латентность ~85с до голоса**: озвучка ждёт конца всего хода; заметную часть съели сломанные инструменты (`gismeteo_weather` без `GISMETEO_TOKEN`, мёртвый MCP `fetch`).

## 2. Объём

Четыре изменения кода + prerequisites. Пункты независимы от порядка, кроме зависимости всего TTS-блока (§5, §6) от спеки №1.

**Prerequisites (ops, вне кода):** задать `GISMETEO_TOKEN` или убрать тул у Arty; починить/убрать MCP `fetch`. Иначе выигрыш латентности частично съедается tool-churn'ом.

## 3. Чистый транскрипт для чата (toolgate)

- `strip_transcript_timecodes` выносится из [summarize_video.py:357](../../../toolgate/handlers/builtin/summarize_video.py) в общий модуль (`toolgate/transcript.py`); summarize_video и term_fixer импортируют оттуда (поведение не меняется).
- `POST /transcribe` ([routers/stt.py:28](../../../toolgate/routers/stt.py)) применяет strip к результату провайдера перед `{"text": …}` — микрофонный путь чата всегда получает чистый текст.
- `POST /transcribe-url` и `ctx.stt` **не меняются**: тайм-коды нужны видео-конспектам и длинным записям.

## 4. Голос во время стриминга → очередь (UI)

- `handleAutoResult` и `handleMicClick` ([ChatComposer.tsx:232-250, 663-677](../../../ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx)): если `isStreaming` — вместо `requestSubmit()` вызывается `queueMessage(text)`; interrupt голосом полностью исключается (решение пользователя: «всегда в очередь», прерывание — только явный Stop).
- `pendingMessage` ([stream-control.ts:104-110](../../../ui/src/stores/chat/actions/stream-control.ts)) расширяется флагом `voice?: true`; повторная голосовая реплика при непустом слоте **дописывается** к тексту через перенос строки (не затирает — пользователь мог наговорить несколько фраз за один ход агента).
- Точка дренажа очереди (ChatThread, idle-effect) при `voice: true` взводит `voiceReplyPendingRef`/`voiceReplyActive` — озвучка ответа работает и для сообщений, ушедших через очередь.
- Индикатор: пока голосовое сообщение в очереди, композер показывает существующий стиль статуса с текстом «отправлю после ответа» (новая i18n-строка).

## 5. Стриминговая озвучка по предложениям (UI, провайдер-агностично)

Заменяет пост-фактум путь целиком: `lastAssistantAudioUrl`, `lastAssistantSpokenText`, `playReply`, `playAudioUrl` и эффект на `isStreaming`-переходе ([ChatComposer.tsx:85-113, 303-369](../../../ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx)) удаляются.

### 5.1 Новый модуль `chat/hooks/tts-speaker.ts`

Чистая логика (DOM-free ядро + тонкий hook, по образцу `vad.ts` / `use-voice-recorder.ts`):

```text
SentenceSplitter: push(delta) → готовые предложения
  - граница: [.!?…] + пробел/EOL, закрывающие кавычки/скобки прилипают
  - min длина 20 символов (короткие фрагменты копятся до границы)
  - flush() на text-end/finish отдаёт остаток
  - markdown-очистка перед синтезом: заголовки/списки/код-блоки → плоский текст,
    код-блоки заменяются на «(код)», ссылки → текст ссылки

SpeakerQueue (состояние: idle | speaking):
  - enqueue(sentence) → fetch POST /api/tts/synthesize?agent={agent} (профиль → провайдер+голос) → blob
  - воспроизведение строго по порядку на едином pre-unlocked <audio> (существующий
    primeTtsAudio-механизм сохраняется);
    синтез следующего предложения идёт ПАРАЛЛЕЛЬНО проигрыванию текущего (prefetch=1)
  - takeoverAudio(url): агентский synthesize_speech-part (SSE `file` event, audio/*) →
    очередь очищается, невоспроизведённые синтезы отменяются (AbortController),
    играет агентское аудио; голос тот же (профиль) — перескок не слышен
  - cancel(): Stop / новый ход / смена агента / unmount → abort всех fetch + pause
  - onDrain: колбэк «очередь пуста и ничего не играет»
```

### 5.2 Подключение

- Источник текста: подписка на live-сообщения текущего голосового хода (voice-turn = ход, начатый голосовой отправкой; флаг живёт там же, где `voiceReplyPendingRef` сейчас). Текстовые дельты ассистента фидятся в splitter; think-блоки и tool-parts не озвучиваются (парсер уже разделяет).
- SSE `file`-event с `mediaType: audio/*` в голосовом ходе → `takeoverAudio(url)`.
- Continuous re-arm ([ChatComposer.tsx:377-381](../../../ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx)): условие `!ttsPlayingRef.current` заменяется на `speaker.idle` (очередь пуста) — микрофон не включается, пока ответ дозвучивает.
- Индикатор `voiceReplyActive`/`ttsPlaying` переезжает на состояние очереди (`preparing` = ход идёт, очередь пуста; `speaking` = очередь играет).
- Не-голосовые ходы: озвучка не запускается вовсе (как сейчас).

### 5.3 Гейтинг по профилю

- `/api/tts/synthesize?agent=` (спека №1) отвечает 409 при пустом tts-слоте профиля → hook отключает озвучку без toast-спама (лог в консоль).
- UI прячет hands-free-тумблер и голосовую индикацию, когда `AgentInfo.capabilities.tts === false`; кнопка микрофона гейтится STT-капабилити (`capabilities.stt`), как сегодня `hasSttProvider`.

## 6. Защита от `(Empty response:` (core)

- Единственная точка нормализации — приём ответа LLM в `pipeline/execute.rs` (где собирается текст ответа итерации): если текст матчит `^\s*\(Empty response:` — заменяется на пустой с `tracing::warn!(provider, model, "upstream serialized empty/thinking-only response as text")`.
- Дальше работает существующая механика пустых ответов (`AutoContinuePolicy.retry_on_empty` — retry один раз, затем штатное завершение) — мусор не персистится, не отображается и не озвучивается.
- Регэксп-паттерн константой рядом с нормализацией, с комментарием-происхождением (ollama-cloud/z.ai-прокси, thinking-only ответ glm-5.1). Корневой фикс апстрима — вне OPEX.

## 7. Тесты

- **toolgate (pytest):** `/transcribe` возвращает текст без `[MM:SS]` (стрим и не-стрим пути провайдера); `/transcribe-url` сохраняет тайм-коды; strip-хелпер общий (summarize_video использует тот же импорт).
- **UI (vitest):**
  - SentenceSplitter: границы (. ! ? … многоточие, кавычки), min-длина, flush, markdown-очистка;
  - SpeakerQueue: порядок воспроизведения, prefetch-параллелизм, cancel, takeoverAudio (очистка очереди + abort), onDrain;
  - очередь-вместо-interrupt: голосовая отправка при streaming кладёт в `pendingMessage` c `voice: true`, дописывание второй реплики, взвод voiceReply на дренаже;
  - гейтинг: `capabilities.tts=false` прячет hands-free; 409 от synthesize отключает озвучку тихо.
- **core:** нормализация `(Empty response:` → пустой текст → retry-путь; обычные ответы не затронуты (негативный тест на текст, содержащий подстроку не в начале).
- **E2E на сервере (ручной прогон):** голосовой ход с инструментами — первый звук до конца хода; ход с `synthesize_speech` — takeover без двойного голоса; ход, где апстрим вернул thinking-дамп — тишина+retry вместо зачитывания мусора.

## 8. Вне объёма

- Barge-in во время проигрывания TTS (перебить голосом — нужна echo cancellation) — сознательно нет: остановка озвучки тапом по микрофону/Stop.
- Streaming-TTS-прокси (chunked audio, MediaSource) — отклонено на брейншторме в пользу провайдер-агностичной нарезки по предложениям через единый API.
- Телеграм/каналы: их голосовой путь (`send_voice` channel_action) не меняется.
