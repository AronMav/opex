---
name: market-analysis
description: Market and investment analysis — asset valuation, portfolio analysis, fundamental and technical analysis
triggers:
  - stocks
  - portfolio
  - investments
  - market
  - stock market
  - dividends
  - multiples
  - P/E
  - company analysis
  - quotes
  - акции
  - портфель
  - инвестиции
  - рынок
  - фондовый рынок
  - дивиденды
  - мультипликаторы
  - анализ компании
  - котировки
priority: 9
tools_required:
  - search_web
state: active
---

## Market Analysis Strategy

### Types of Analysis

#### Fundamental Company Analysis
1. **Financial metrics**: revenue, net income, EBITDA, margins
2. **Multiples**: P/E, P/S, P/BV, EV/EBITDA, dividend yield
3. **Debt load**: Debt/Equity, Net Debt/EBITDA
4. **Dynamics**: YoY revenue growth, profit growth, margin trend
5. **Sector comparison**: position among competitors

#### Technical Analysis
1. **Trend**: direction, key support/resistance levels
2. **Volume**: anomalous spikes, trend confirmation
3. **Indicators**: RSI, MACD (descriptive only, do not calculate)

#### Portfolio Analysis
1. **Diversification**: by sector, currency, asset type
2. **Concentration**: share of largest positions
3. **Risk**: beta coefficient, maximum drawdown
4. **Return**: absolute and relative to benchmark

### Data Sources

- `search_web` for current quotes and news
- `search_web` for historical data and analytics
- MOEX, Investing.com, SmartLab — primary sources for Russian market
- Yahoo Finance, Finviz — for international markets

### Report Format

```
## Analysis: [company/asset/portfolio]

### Current Situation
Price, dynamics, key events

### Fundamental Metrics
Table of key multiples

### Risks
- Company-specific
- Industry-level
- Macroeconomic

### Conclusions and Recommendations
Reasoned outlook assessment
```

### Important
- Always state the date of data — market information becomes stale quickly
- Do not give direct "buy/sell" recommendations — provide analysis for decision-making
- Clearly separate facts from interpretations
- If data is insufficient — say so
