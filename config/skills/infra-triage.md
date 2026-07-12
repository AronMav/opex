---
name: infra-triage
description: Диагностика и ремонт проблемных docker-контейнеров по триггеру Watchdog; safe чинит сам, при сомнении спрашивает владельца
triggers:
  - infra-triage
  - Infra
  - проблемный контейнер
  - docker-контейнер
tools_required:
  - code_exec
priority: 20
---

# Infra Triage — self-healing docker-контейнеров

Тебя вызвал Watchdog: обнаружен устойчиво-проблемный контейнер. Действуй по протоколу.
Все HTTP-вызовы — Python `requests` в `code_exec`, база `http://localhost:18789`,
заголовок `Authorization: Bearer <OPEX_AUTH_TOKEN из env>`. НЕ используй curl.

## ГЛАВНОЕ ПРАВИЛО (читай первым)

По проблемному контейнеру **уже создано** pending-решение (его id указан в затравке
`[Infra] … pending-решение <id>`), и владелец **уже уведомлён** кнопками
«Выполнить»/«Отклонить». Твоя работа — **дополнить или резолвить ЭТО решение** через
**`PATCH /api/infra/decisions/<id>`** (`code_exec`, Python `requests`).

- **НЕ создавай** новое решение (`POST`) — оно уже есть.
- **НЕ пиши** владельцу текстом / `send_message` — уведомление ушло автоматически из
  pending-записи; текст в чат кнопок не даёт и дублирует.
- Итог прогона = один `PATCH <id>` (обновление содержимого и/или статус). Если ты
  ничего не сделаешь — владелец всё равно увидит вопрос (базовый pending), а
  анти-петля не даст перезапускать тебя. Но твоя диагностика полезна — доведи её до
  PATCH.

## 1. Диагностика (измеряй, не гадай)

- `docker inspect <name>` — состояние, ExitCode, ошибка старта.
- Сверь с активным `~/opex/docker/docker-compose.yml`: есть ли такой сервис,
  закомментирован ли он.
- Сверь с активными провайдерами: `GET /api/providers` — используется ли сервис
  (порт/URL) как активный провайдер.
- Проверь порт: `ss -ltnp | grep <порт>` — слушает ли кто-то.

## 2. Классифицируй и заверши через PATCH <id>

**SAFE — чини сам:** контейнер, который ДОЛЖЕН работать и просто упал (известный
compose-сервис в `Exited`/`Restarting`, всё ещё нужный). Действие: `docker restart
<name>`, затем `PATCH <id> {status:"done", proposed_action:"restarted"}`.

**СОМНЕНИЕ — оставь владельцу:** удаление (`docker rm`), правка `compose`, незнакомый
контейнер. НЕ выполняй сам. `PATCH <id>` с уточнённым `diagnosis` и точными
`proposed_commands` (список шагов) — **статус НЕ меняй**, оставь pending. Владелец уже
видит кнопки; при «Выполнить» тебя перезапустят выполнить твои `proposed_commands`.

**НИЧЕГО НЕ ТРЕБУЕТСЯ** (штатно остановлен, ложная тревога): `PATCH <id>
{status:"dismissed", proposed_action:"<почему ок>"}`.

## 3. Шаблон вызова (code_exec)

```python
import os, requests
requests.patch(
    "http://localhost:18789/api/infra/decisions/<id>",   # <id> из затравки [Infra]
    headers={"Authorization": "Bearer " + os.environ["OPEX_AUTH_TOKEN"]},
    json={
        # любой набор полей; пропущенные не меняются:
        "diagnosis": "<что измерил: состояние, в compose ли, слушает ли порт>",
        "proposed_action": "<человекочитаемо>",
        "proposed_commands": ["docker rm ..."],   # для случая СОМНЕНИЕ
        "status": "done",                          # done | dismissed | failed; опусти, чтобы оставить pending
    },
    timeout=10,
).raise_for_status()
```

Заверши прогон одним таким PATCH. Свободный ответ в чат его НЕ заменяет (см. Главное
правило).

## 4. Исполнение одобренного (когда тебя вызвали с «Владелец одобрил решение …»)

Выполни `proposed_commands` дословно через `code_exec` (а если их не было — сам
продиагностируй и выполни необходимое). Если среди шагов есть правка серверного
`~/opex/docker/docker-compose.yml` — **предупреди владельца в отчёте**, что git-версия
compose разошлась и её нужно обновить (deploy не синкает docker/). По завершении:
`PATCH /api/infra/decisions/<id> {status:"done"}` (или `"failed"` при сбое).

## Никогда

- Не трогай `postgres` и контейнеры с данными.
- Не удаляй и не правь compose без явного «да» владельца.
- Не отвечай владельцу текстом/`send_message` вместо `PATCH /api/infra/decisions/<id>`.
- Не interpretируй — измеряй; фиксируй симптом.
