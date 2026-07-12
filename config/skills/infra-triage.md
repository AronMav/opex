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

## 1. Диагностика (измеряй, не гадай)

- `docker inspect <name>` — состояние, ExitCode, ошибка старта.
- Сверь с активным `~/opex/docker/docker-compose.yml`: есть ли такой сервис,
  закомментирован ли он.
- Сверь с активными провайдерами: `GET /api/providers` — используется ли сервис
  (порт/URL) как активный провайдер.
- Проверь порт: `ss -ltnp | grep <порт>` — слушает ли кто-то.

## 2. Классифицируй и действуй

**SAFE — чини сам, без вопроса:** контейнер, который ДОЛЖЕН работать и просто упал
(известный compose-сервис в `Exited`/`Restarting`, всё ещё нужный). Действие:
`docker restart <name>`. Затем зафиксируй результат:
`POST /api/infra/decisions {container, diagnosis, proposed_action:"restarted",
proposed_commands:[], status:"done"}`.

**СОМНЕНИЕ — спрашивай владельца:** удаление контейнера (`docker rm`), правка
`compose`, любой незнакомый контейнер. НЕ выполняй сам. Создай вопрос:
`POST /api/infra/decisions {container, diagnosis:"<что выяснил>",
proposed_action:"<человекочитаемо>", proposed_commands:["docker rm ...", "..."],
status:"pending"}`. Владелец подтвердит — тебя перезапустят с командой выполнить.

**НИЧЕГО НЕ ТРЕБУЕТСЯ** (контейнер штатно остановлен, ложная тревога):
`POST /api/infra/decisions {..., proposed_action:"<почему ок>", status:"dismissed"}`.

## 3. Обязательный итог

По итогу ОБЯЗАТЕЛЬНО оставь ровно одну запись в `infra_decisions`
(`pending` | `done` | `dismissed`). Молчаливое завершение без записи ломает
анти-петлевой дебаунс — Watchdog будет дёргать тебя снова и снова.

## 4. Исполнение одобренного (когда тебя вызвали с «Владелец одобрил решение …»)

Выполни `proposed_commands` дословно через `code_exec`. Если среди шагов есть правка
серверного `~/opex/docker/docker-compose.yml` — **предупреди владельца в отчёте**, что
git-версия compose разошлась и её нужно обновить (deploy не синкает docker/). По
завершении: `PATCH /api/infra/decisions/{id} {status:"done"}` (или `"failed"` при сбое).

## Никогда

- Не трогай `postgres` и контейнеры с данными.
- Не удаляй и не правь compose без явного «да» владельца.
- Не interpretируй — измеряй; фиксируй симптом.
