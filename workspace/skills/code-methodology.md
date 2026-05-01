---
name: code-methodology
description: Development methodology — TDD, code review, refactoring, debugging
triggers:
  - write code
  - test
  - bug
  - debugging
  - refactoring
  - code review
  - review code
  - check the code
  - find bugs
  - debug
  - напиши код
  - тест
  - баг
  - отладка
  - рефакторинг
  - ревью кода
  - проверь код
  - найди баги
  - посмотри код
  - дебаг
priority: 5
tools_required:
  - code_exec
state: active
---

## Code Development Methodology

### TDD (Test-Driven Development)

When the user asks to write or fix code:

1. **Red** — write a failing test (describes expected behavior)
2. **Green** — write the minimum code to make the test pass
3. **Refactor** — improve the code without breaking tests

### Debugging

When something doesn't work:

1. **Reproduce** — can you repeat the error?
2. **Isolate** — narrow the search area (file → function → line)
3. **Hypothesize** — what could be the cause?
4. **Verify** — test the hypothesis, don't "fix" blindly
5. **Fix** — the minimal change that solves the problem
6. **Confirm** — verify the fix works and nothing else broke

### Code Review

When reviewing code, check:

- **Security** — SQL injection, XSS, command injection, hardcoded secrets
- **Bugs** — null/undefined handling, off-by-one, race conditions
- **Correctness** — does the code do what it claims?
- **Edge cases** — null, empty strings, 0, negative numbers
- **Error handling** — what happens if the API doesn't respond? File not found?
- **Performance** — N+1 queries, unnecessary allocations, missing indexes, O(n²) where O(n) is possible
- **Readability** — is the code understandable without comments?
- **Style** — naming conventions, dead code, code duplication

Rate severity: **CRITICAL** / **HIGH** / **MEDIUM** / **LOW**

### Principles

- **KISS** — the simplest solution that works
- **DRY** — don't repeat yourself, but don't abstract prematurely
- **YAGNI** — don't write what "might be useful"
- **Fail fast** — errors should surface early and explicitly
- **Immutability** — prefer immutable data

### Code Review Response Format

```
## Overall Rating: ✅ / ⚠️ / ❌

### Summary
One sentence: overall quality assessment

### Issues Found
1. **[SEVERITY]** Brief description
   - Location: file:line
   - Fix: specific code change

### Notes
1. [file:line] — description → recommendation

### What's Good
- 2-3 positive observations (reinforcement)
```

### Interface-First Ordering

When building multi-step features, define contracts before implementations:

1. **First: Define contracts** -- Create type files, interfaces, API schemas, exported signatures
2. **Middle: Implement** -- Build each component against the defined contracts
3. **Last: Wire** -- Connect implementations to consumers

**Why this order matters:**
- Prevents the "scavenger hunt" -- later steps know exact types without exploring the codebase
- Enables parallel implementation -- two components can be built simultaneously against shared types
- Catches design mismatches early -- if the interface does not fit, you discover it before writing code

**Example:**
```
Step 1: Create src/types/feature.ts (interfaces + exported types)
Step 2: Create src/api/feature.ts (implements API against types)
Step 3: Create src/components/Feature.tsx (consumes API, uses types)
Step 4: Wire Feature into page layout
```

### Contract-Driven Development

Extend TDD principles to system boundaries:

**Contracts define the boundaries between components.** Before writing implementation code, answer:

- What data crosses this boundary? (types, schemas)
- What can the consumer call? (function signatures, API endpoints)
- What errors can occur? (error types, status codes)
- What side effects happen? (events emitted, state changes)

**Contract-first workflow:**

1. **Write the contract** -- types, interfaces, API schema
2. **Write tests against the contract** -- test expected inputs produce expected outputs
3. **Implement to satisfy the contract** -- minimal code that passes tests
4. **Verify at the boundary** -- integration test that crosses the boundary

**When contracts matter most:**
- API endpoints (request/response types)
- Component props (what the parent provides)
- Module exports (public surface area)
- Event schemas (what subscribers receive)

**When contracts are overkill:**
- Internal helper functions (private, single caller)
- Configuration files (static, no consumers)
- One-off scripts (run once, discard)

This extends the TDD approach already described above -- Red/Green/Refactor applies at the boundary level, not just the function level. Use `skill_use("verification")` for adversarial testing of contract implementations.
