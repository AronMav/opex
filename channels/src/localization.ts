/**
 * Localized UI strings for channel adapters.
 * Port of crates/opex-channel/src/localization.rs
 */

export interface Strings {
  accessRestricted(code: string): string;
  accessRequest(label: string, userId: string, code: string): string;
  documentsNotSupported: string;
  userApproved(info: string): string;
  codeExpired: string;
  codeNotFound: string;
  requestRejected: string;
  noApprovedUsers: string;
  approvedUsersHeader: string;
  userListItem(label: string, uid: string, date: string): string;
  revokeHint: string;
  userRevoked(id: string): string;
  userNotFound: string;
  errorMessage(err: string): string;
  // Approval system
  approvalHeader(toolName: string): string;
  approvalApprove: string;
  approvalReject: string;
  approvalApproved: string;
  approvalRejected: string;
  approvalForbidden: string;
  actionUnavailable: string;
  // Commands
  noActiveRequest: string;
  thinkModeOff: string;
  thinkModeOn: string;
  stopped: string;
  choose: string;
  // Bot command descriptions (Telegram menu)
  cmdHelp: string;
  cmdStatus: string;
  cmdMemory: string;
  cmdNew: string;
  cmdCompact: string;
  cmdStop: string;
  cmdThink: string;
}

const RU: Strings = {
  // F122: plain text (no Telegram MarkdownV2 escaping) — these strings are shared
  // across Matrix/Slack/IRC/Email, which render them literally. The Telegram
  // driver sends them without parse_mode:"MarkdownV2".
  accessRestricted: (code) =>
    `Доступ ограничен. Ваш код: ${code}\nПередайте его владельцу бота.`,
  accessRequest: (label, userId, code) =>
    `Запрос доступа от ${label} (ID: ${userId})\nКод: ${code}\n\n/approve ${code}`,
  documentsNotSupported: "Документы пока не поддерживаются.",
  userApproved: (info) => `Пользователь ${info} одобрен.`,
  codeExpired: "Код устарел. Попросите пользователя отправить сообщение снова.",
  codeNotFound: "Код не найден.",
  requestRejected: "Запрос отклонён.",
  noApprovedUsers: "Нет одобренных пользователей (кроме владельца).",
  approvedUsersHeader: "Одобренные пользователи:\n\n",
  userListItem: (label, uid, date) => `- ${label} (ID: ${uid}, с ${date})\n`,
  revokeHint: "\n/revoke ID — отозвать доступ",
  userRevoked: (id) => `Доступ пользователя ${id} отозван.`,
  userNotFound: "Пользователь не найден или ошибка.",
  errorMessage: (err) => `Ошибка: ${err}`,
  approvalHeader: (tool) => `🔐 Подтвердите действие: ${tool}`,
  approvalApprove: "✅ Разрешить",
  approvalReject: "❌ Отклонить",
  approvalApproved: "✅ Разрешено",
  approvalRejected: "❌ Отклонено",
  approvalForbidden: "Только владелец может подтверждать действия.",
  actionUnavailable: "Это действие больше недоступно — откройте файл заново, чтобы увидеть действия.",
  noActiveRequest: "Нет активного запроса.",
  thinkModeOff: "🧠 Режим размышлений выключен.",
  thinkModeOn: "🧠 Режим размышлений включён для следующего сообщения.",
  stopped: "Остановлено.",
  choose: "Выберите:",
  cmdHelp: "Показать команды",
  cmdStatus: "Статус агента",
  cmdMemory: "Поиск в памяти",
  cmdNew: "Новая сессия",
  cmdCompact: "Сжать контекст",
  cmdStop: "Остановить обработку",
  cmdThink: "Режим размышлений",
};

const EN: Strings = {
  // F122: plain text — see the RU note above.
  accessRestricted: (code) =>
    `Access restricted. Your code: ${code}\nSend it to the bot owner.`,
  accessRequest: (label, userId, code) =>
    `Access request from ${label} (ID: ${userId})\nCode: ${code}\n\n/approve ${code}`,
  documentsNotSupported: "Documents are not supported yet.",
  userApproved: (info) => `User ${info} approved.`,
  codeExpired: "Code expired. Ask the user to send a message again.",
  codeNotFound: "Code not found.",
  requestRejected: "Request rejected.",
  noApprovedUsers: "No approved users (besides the owner).",
  approvedUsersHeader: "Approved users:\n\n",
  userListItem: (label, uid, date) => `- ${label} (ID: ${uid}, since ${date})\n`,
  revokeHint: "\n/revoke ID — revoke access",
  userRevoked: (id) => `Access for user ${id} revoked.`,
  userNotFound: "User not found or error.",
  errorMessage: (err) => `Error: ${err}`,
  approvalHeader: (tool) => `🔐 Approve action: ${tool}`,
  approvalApprove: "✅ Approve",
  approvalReject: "❌ Reject",
  approvalApproved: "✅ Approved",
  approvalRejected: "❌ Rejected",
  approvalForbidden: "Only the owner can resolve approvals.",
  actionUnavailable: "This action is no longer available — re-open the file to see actions.",
  noActiveRequest: "No active request.",
  thinkModeOff: "🧠 Think mode off.",
  thinkModeOn: "🧠 Think mode on for next message.",
  stopped: "Stopped.",
  choose: "Choose:",
  cmdHelp: "Show commands",
  cmdStatus: "Agent status",
  cmdMemory: "Memory search",
  cmdNew: "New session",
  cmdCompact: "Compact context",
  cmdStop: "Stop processing",
  cmdThink: "Think mode",
};

export function getStrings(language: string): Strings {
  return language === "en" ? EN : RU;
}
