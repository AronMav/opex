//! Localized user-facing strings for agent commands.

/// Sequential placeholder replacement: replaces `{}` one at a time, left to right.
pub fn fmt(template: &str, values: &[&str]) -> String {
    let mut result = template.to_string();
    for value in values {
        if let Some(pos) = result.find("{}") {
            result.replace_range(pos..pos + 2, value);
        }
    }
    result
}

pub struct CommandStrings {
    pub status_session_active: &'static str,
    pub status_session_none: &'static str,
    pub status_format: &'static str,
    pub new_session_started: &'static str,
    pub new_session_none: &'static str,
    pub reset_done: &'static str,
    pub compact_no_session: &'static str,
    pub compact_done: &'static str,
    pub compact_not_needed: &'static str,
    pub model_current: &'static str,
    pub model_override: &'static str,
    pub model_reset: &'static str,
    pub model_switched: &'static str,
    pub think_level: &'static str,
    pub usage_header: &'static str,
    pub usage_session: &'static str,
    pub export_no_session: &'static str,
    pub export_empty: &'static str,
    pub export_header: &'static str,
    pub memory_empty: &'static str,
    pub memory_header: &'static str,
    pub help_text: &'static str,
}

pub struct ErrorStrings {
    pub context_overflow: &'static str,
    pub session_corruption: &'static str,
    pub transient_http: &'static str,
    pub rate_limit: &'static str,
    pub auth_permanent: &'static str,
    pub billing: &'static str,
    pub overloaded: &'static str,
    pub unknown: &'static str,
}

const RU: CommandStrings = CommandStrings {
    status_session_active: "Сессия: активна ({} сообщений)",
    status_session_none: "Сессия: нет",
    status_format: "📊 Агент: {}\nПровайдер: {}\nМодель: {}\n{}\nПамять: {} чанков",
    new_session_started: "🔄 Новая сессия начата.",
    new_session_none: "🔄 Нет активной сессии — новая будет создана при следующем сообщении.",
    reset_done: "🗑️ Сессия очищена. Удалено {} чанков памяти (закреплённые сохранены).",
    compact_no_session: "Нет активной сессии для сжатия.",
    compact_done: "📦 Сжато: {} → {} сообщений, извлечено {} фактов в память.",
    compact_not_needed: "Контекст достаточно мал, сжатие не требуется.",
    model_current: "🤖 Модель: `{}`",
    model_override: "🤖 Модель: `{}` (override)\nБазовая: `{}`",
    model_reset: "🤖 Модель сброшена на `{}`",
    model_switched: "🤖 Модель переключена на `{}`",
    think_level: "💭 Уровень мышления: {} ({}/5)",
    usage_header: "📊 *Использование ({})*\n\nСегодня: {}→{} токенов ({} вызовов)",
    usage_session: "Сессия: {}→{} токенов ({} вызовов)",
    export_no_session: "Нет активной сессии для экспорта.",
    export_empty: "Сессия пуста.",
    export_header: "# Экспорт сессии\n**Агент**: {} | **Сессия**: {}\n\n---\n",
    memory_empty: "Память пуста или ничего не найдено.",
    memory_header: "🧠 Память ({}, {} результатов):\n\n{}",
    help_text: "📋 *Доступные команды:*\n\n\
/status — текущее состояние агента\n\
/new — начать новую сессию\n\
/reset — очистить сессию и память\n\
/compact — сжать историю\n\
/memory [запрос] — поиск в памяти\n\
/model [имя|reset] — показать/сменить модель\n\
/think [on|off] — уровень мышления\n\
/usage — статистика токенов\n\
/export — экспорт сессии\n\
/help — эта справка",
};

const EN: CommandStrings = CommandStrings {
    status_session_active: "Session: active ({} messages)",
    status_session_none: "Session: none",
    status_format: "📊 Agent: {}\nProvider: {}\nModel: {}\n{}\nMemory: {} chunks",
    new_session_started: "🔄 New session started.",
    new_session_none: "🔄 No active session — a new one will be created on next message.",
    reset_done: "🗑️ Session cleared. Removed {} memory chunks (pinned kept).",
    compact_no_session: "No active session to compact.",
    compact_done: "📦 Compacted: {} → {} messages, extracted {} facts to memory.",
    compact_not_needed: "Context is small enough, no compaction needed.",
    model_current: "🤖 Model: `{}`",
    model_override: "🤖 Model: `{}` (override)\nBase: `{}`",
    model_reset: "🤖 Model reset to `{}`",
    model_switched: "🤖 Model switched to `{}`",
    think_level: "💭 Thinking level: {} ({}/5)",
    usage_header: "📊 *Usage ({})*\n\nToday: {}→{} tokens ({} calls)",
    usage_session: "Session: {}→{} tokens ({} calls)",
    export_no_session: "No active session to export.",
    export_empty: "Session is empty.",
    export_header: "# Session Export\n**Agent**: {} | **Session**: {}\n\n---\n",
    memory_empty: "Memory is empty or nothing found.",
    memory_header: "🧠 Memory ({}, {} results):\n\n{}",
    help_text: "📋 *Available commands:*\n\n\
/status — agent status\n\
/new — start new session\n\
/reset — clear session and memory\n\
/compact — compact history\n\
/memory [query] — search memory\n\
/model [name|reset] — show/change model\n\
/think [on|off] — thinking level\n\
/usage — token statistics\n\
/export — export session\n\
/help — this help",
};

const RU_ERR: ErrorStrings = ErrorStrings {
    context_overflow: "Контекст слишком большой для модели. Попробуйте начать новую сессию.",
    session_corruption: "Сессия повреждена. Начните новый чат.",
    transient_http: "Временная ошибка сервера. Попробуйте ещё раз через минуту.",
    rate_limit: "Слишком много запросов. Подождите немного.",
    auth_permanent: "Ошибка аутентификации API. Проверьте ключ.",
    billing: "Проблема с оплатой/квотой API провайдера.",
    overloaded: "Сервер перегружен. Попробуйте позже.",
    unknown: "Произошла ошибка. Попробуйте ещё раз.",
};

const EN_ERR: ErrorStrings = ErrorStrings {
    context_overflow: "Context is too large for the model. Try starting a new session.",
    session_corruption: "Session is corrupted. Please start a new chat.",
    transient_http: "Temporary server error. Please try again in a minute.",
    rate_limit: "Too many requests. Please wait a moment.",
    auth_permanent: "API authentication error. Please check your key.",
    billing: "API provider billing/quota issue.",
    overloaded: "Server is overloaded. Please try later.",
    unknown: "An error occurred. Please try again.",
};

pub fn get_strings(language: &str) -> &'static CommandStrings {
    if language == "en" { &EN } else { &RU }
}

pub fn get_error_strings(language: &str) -> &'static ErrorStrings {
    if language == "en" { &EN_ERR } else { &RU_ERR }
}
