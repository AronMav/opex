import { describe, test, expect } from "bun:test";

// ── Email Driver ────────────────────────────────────────────────────────
// createEmailDriver wraps ImapFlow + nodemailer — both require real network
// connections and are not unit-testable without a live IMAP/SMTP server.
// Behaviour covered here via todo stubs; integration coverage lives in E2E.

describe("Email Driver", () => {
  test.todo("createEmailDriver returns start/stop/onAction functions");
  test.todo("pollInbox marks messages as Seen after processing");
  test.todo("pollInbox sends pairing code when user is not allowed");
  test.todo("pollInbox passes parseDirectives result to sendMessage");
  test.todo("onAction reply calls sendEmail with correct to/subject");
  test.todo("stop sets stopPolling flag and calls client.logout");
});
