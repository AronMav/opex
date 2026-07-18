> **Язык:** [English](CONTRIBUTING.md) · Русский

# Участие в разработке OPEX

Спасибо за интерес к проекту! Вот как начать.

## Начало работы

1. Создайте форк репозитория
2. Клонируйте форк: `git clone https://github.com/AronMav/opex`
3. Создайте ветку: `git checkout -b feature/your-feature-name`

## Настройка окружения разработки

### Зависимости

- Rust 1.85+ (`rustup update stable`)
- PostgreSQL 17 с расширением pgvector
- Bun 1.x (для адаптеров каналов)
- Python 3.11+ с uv (для toolgate)

### Локальный запуск

```bash
# 1. Запустить PostgreSQL
docker compose -f docker/docker-compose.yml up -d postgres

# 2. Настроить окружение
cp .env.example .env
# Отредактируйте .env, указав ваши значения

# 3. Собрать и запустить
cargo run -p opex-core

# 4. (Опционально) Запустить адаптеры каналов
cd channels && bun install && bun run src/index.ts
```

### Запуск тестов

```bash
# Все тесты
make test

# Один тест
cargo test test_name -- --nocapture

# Тесты UI
cd ui && npm test

# Тесты адаптеров каналов
cd channels && bun test
```

### Линтинг

```bash
make lint          # cargo clippy --all-targets -- -D warnings
cd ui && npm run typecheck
```

## Стиль кода

### Rust

- Следуйте стандартным идиомам Rust (`cargo clippy` должен проходить с `-D warnings`)
- Используйте `anyhow` для передачи ошибок в прикладном коде, `thiserror` для библиотечных ошибок
- Никакого `unwrap()` или `expect()` в продакшн-путях — используйте `?` или корректную обработку ошибок
- Все зависимости должны использовать `rustls-tls` (без OpenSSL) для возможности кросс-компиляции

### TypeScript

- Включён строгий режим — никаких типов `any`
- Следуйте существующим паттернам в кодовой базе

### YAML-инструменты

При добавлении нового инструмента в `workspace/tools/`:

- `description` должен быть на английском и чётко объяснять, когда использовать инструмент
- Устанавливайте `status: draft` до тестирования, `status: verified` после подтверждения работоспособности
- Тестируйте все параметры перед отправкой

## Отправка Pull Request

1. Убедитесь, что тесты проходят: `make test && make lint`
2. Держите PR сфокусированным — один feature или fix на PR
3. Пишите чёткое описание PR, объясняя что и почему
4. Ссылайтесь на связанные issues

## Сообщение об ошибках

При сообщении об ошибке укажите:

- Версию OPEX или хэш коммита
- Операционную систему и архитектуру
- Соответствующие логи (из `journalctl` или stdout)
- Шаги для воспроизведения

## Уязвимости безопасности

Пожалуйста, **не открывайте** публичные issues для уязвимостей безопасности. Вместо этого создайте [GitHub Security Advisory](https://github.com/AronMav/opex/security/advisories/new) или напишите напрямую мейнтейнерам.

## Создание Release

```bash
# Сборка release-архива (все платформы)
./release.sh 0.27.0 --all

# Результат: release/opex-v0.27.0.tar.gz
```

Скрипт release синхронизирует версию в `Cargo.toml` и файлы `package.json`, собирает все бинарники, упаковывает UI и создаёт единый архив.

Для публикации release на GitHub создайте и запушьте тег — CI собирает и публикует автоматически:

```bash
git tag v0.27.0
git push origin v0.27.0
```

## Вопросы

Открывайте [GitHub Discussion](https://github.com/AronMav/opex/discussions) для вопросов об использовании, архитектуре или решениях по дизайну.
