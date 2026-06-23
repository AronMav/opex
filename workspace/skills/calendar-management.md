---
name: calendar-management
description: Calendar management — viewing schedule, creating events, reminders
status: draft
triggers:
  - calendar
  - schedule
  - meeting
  - event
  - schedule this
  - what do I have today
  - appointment
  - календарь
  - расписание
  - встреча
  - событие
  - запланируй
  - что у меня сегодня
  - запись
priority: 8
tools_required:
  - calendar_today
  - calendar_upcoming
  - calendar_create
state: active
---

## Calendar Management Strategy

### Viewing the schedule
- "What's today?" → `calendar_today`
- "What's this week?" → `calendar_upcoming(days=7)`
- "What's tomorrow?" → `calendar_upcoming(days=1)`
- Show times as HH:MM, without seconds
- Group by day if spanning multiple days

### Creating events
BEFORE creating, show the user and wait for confirmation:
- **Event**: name
- **When**: date and time (start — end)
- **Where**: location (if specified)

If the user did not specify an end time — default to +1 hour.
Default timezone: Europe/Samara.

### Reminders
Use cron jobs for reminders, not calendar events:
- "Remind me in 2 hours" → cron job
- "Schedule a meeting at 15:00" → calendar event

### Status
Tools are in draft mode — to activate:
1. Create a Google Cloud project + enable Calendar API
2. Create a Service Account, download the JSON key
3. Share the calendar with the service account email
4. Place the JSON key on Pi: ~/opex/config/google-service-account.json
5. Add GOOGLE_CALENDAR_ID to secrets
6. Verify tools via UI
