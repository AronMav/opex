---
name: email-management
description: Email management — checking inbox, sending, searching messages
status: draft
triggers:
  - email
  - inbox
  - letter
  - send email
  - check email
  - unread
  - mail
  - почта
  - входящие
  - письмо
  - отправь письмо
  - проверь почту
  - непрочитанные
priority: 8
tools_required:
  - email_check
  - email_send
  - email_search
state: active
---

## Email Management Strategy

### Checking email
1. `email_check(unread_only=true)` — unread messages
2. Group by importance: from people > automated > newsletters
3. Briefly describe each: from whom, about what (1 line)

### Sending email
ALWAYS show the user and wait for confirmation before sending:
- **To**: address
- **Subject**: subject
- **Body**: content

Without an explicit "yes" / "send it" — do NOT send.

### Search
- `email_search(query="topic")` — searches subject and body
- Show relevant results with date and sender

### Status
Tools are in draft mode — to activate:
1. Add secrets EMAIL_USER and EMAIL_PASS to vault
2. For Gmail: use App Password (not regular password)
3. Verify tools via UI
