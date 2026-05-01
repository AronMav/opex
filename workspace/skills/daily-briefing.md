---
name: daily-briefing
description: Morning briefing with weather, news, portfolio, and calendar
triggers:
  - morning briefing
  - daily briefing
  - what's new
  - digest
  - утренний брифинг
  - ежедневный брифинг
  - что нового
  - дайджест
  - сводка
state: active
---

# Daily Briefing Skill

## Execution Plan

Execute these in parallel where possible:

1. **Weather**: get_weather for user city
2. **Portfolio**: bcs_portfolio to get current positions, then get_stock_price for each ticker
3. **Market news**: search_web_fresh for Russian stock market today
4. **Currency**: get_cbr_rate for USD and EUR
5. **General news**: search_web_fresh for top 3 news stories

## Output Format

Start with greeting and date/time, then sections:
- Weather
- Portfolio (table with P&L)
- Currencies
- Top News (3-5 headlines with 1-sentence summaries)

Keep it concise — this is a morning scan, not deep analysis.