---
name: code-methodology
description: Development methodology — TDD, debugging, code review, refactoring, and adversarial verification for comprehensive code quality assurance
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
  - verify
  - check my work
  - is this correct
  - test this
  - validation
  - проверь
  - верификация
  - протестируй
priority: 7
tools_required:
  - code_exec
state: active
pinned: true
---

## Code Development Methodology

This skill combines disciplined development practices (TDD, debugging, code review, contract-first design) with an **adversarial verification protocol** that treats every check as an attempt to break the code, not confirm it works.

---

### Core Principles

- **KISS** — the simplest solution that works
- **DRY** — don't repeat yourself, but don't abstract prematurely
- **YAGNI** — don't write what "might be useful"
- **Fail fast** — errors should surface early and explicitly
- **Immutability** — prefer immutable data

---

### TDD (Test-Driven Development)

When writing or fixing code:

1. **Red** — write a failing test that describes the expected behavior
2. **Green** — write the minimum code to make the test pass
3. **Refactor** — improve the code without breaking tests

After the green phase, use the **Adversarial Verification** protocol (below) to try to break the implementation before moving on.

---

### Contract-Driven Development

Extend TDD principles to system boundaries. Define contracts before implementations.

**Interface-First Ordering (multi-step features):**

1. **Define contracts** — type files, interfaces, API schemas, exported signatures
2. **Implement** — build each component against the defined contracts
3. **Wire** — connect implementations to consumers

**Why this order matters:**
- Prevents the "scavenger hunt" — later steps know exact types without exploring the codebase
- Enables parallel implementation — two components can be built simultaneously against shared types
- Catches design mismatches early — if the interface does not fit, you discover it before writing code

**Contract-first workflow:**

1. **Write the contract** — types, interfaces, API schema
2. **Write tests against the contract** — test expected inputs produce expected outputs
3. **Implement to satisfy the contract** — minimal code that passes tests
4. **Verify at the boundary** — integration test that crosses the boundary, then apply the Adversarial Verification protocol

**When contracts matter most:**
- API endpoints (request/response types)
- Component props (what the parent provides)
- Module exports (public surface area)
- Event schemas (what subscribers receive)

**When contracts are overkill:**
- Internal helper functions (private, single caller)
- Configuration files (static, no consumers)
- One-off scripts (run once, discard)

---

### Debugging

When something doesn't work:

1. **Reproduce** — can you repeat the error?
2. **Isolate** — narrow the search area (file → function → line)
3. **Hypothesize** — what could be the cause?
4. **Verify** — test the hypothesis, don't "fix" blindly
5. **Fix** — the minimal change that solves the problem
6. **Confirm** — verify the fix works and nothing else broke (use Adversarial Verification)

---

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

**Code Review Response Format:**

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

---

### Adversarial Verification Protocol

This protocol is used whenever you need to verify code — after TDD green, during code review, or when the user asks to "verify", "check my work", "test this", etc.

#### Core Principle

Your job is to try to **BREAK** it, not confirm it works. Approach every verification as a hostile reviewer. If you find yourself thinking "this looks right" — you are doing it wrong. Find the evidence or declare it unverified.

#### Anti-Rationalization Warnings

STOP if you catch yourself:

- "This probably works because..." — probably is not evidence
- "The code looks correct" — reading is not testing
- "It should handle that case" — should is not does
- "I already checked this" — show the output or check again
- Skipping edge cases because the happy path worked
- Declaring PASS without running a single command

The goal is a verdict backed by exact evidence, not a feeling of confidence.

#### Verification Process

**Step 1: Define Success Criteria**

Before touching anything, write down what must be TRUE for this to pass. Use concrete, observable statements:

- BAD: "The API works correctly"
- GOOD: "POST /api/items returns 201 with {id, name, createdAt} when given valid {name} body"
- GOOD: "Invalid email input shows red border and 'Invalid email' text below the field"

Each criterion needs:
- **Input** — what you provide
- **Expected output** — what you observe
- **How to check** — the exact command, URL, or action

**Step 2: Verify the Happy Path**

Run the expected scenario. Capture exact output in this format:

EVIDENCE: [paste exact command and output]
CRITERION: [which criterion this satisfies]
RESULT: PASS / FAIL

Do NOT proceed to edge cases if the happy path fails. Fix first.

**Step 3: Try to Break It**

For each verified criterion, attempt to violate it:

*Input attacks:*
- Empty input, null, undefined
- Extremely long input (10K+ characters)
- Special characters: script tags, SQL injection, path traversal
- Wrong types: string where number expected, array where object expected
- Boundary values: 0, -1, MAX_INT, empty array

*State attacks:*
- Call without authentication
- Call with expired/invalid token
- Call twice rapidly (race condition)
- Call with stale data
- Call after the resource was deleted

*Environment attacks:*
- Network timeout (does it hang forever?)
- Database unavailable (does it crash or return error?)
- File not found (does it create or fail gracefully?)
- Permission denied (does it surface the error?)

**Step 4: Check What Was NOT Tested**

List every criterion from Step 1. Mark each:
- **TESTED** — with evidence reference
- **NOT TESTED** — with reason

Untested criteria are PARTIAL, not PASS.

**Step 5: Render Verdict**

Use this format:

## Verification Verdict

**Overall: PASS | FAIL | PARTIAL**

| # | Criterion | Result | Evidence |
|---|-----------|--------|----------|
| 1 | ...       | PASS   | See E1   |
| 2 | ...       | FAIL   | See E2   |

### Issues Found
- [list any failures with reproduction steps]

### Untested Areas
- [list anything not covered]

### Confidence
HIGH / MEDIUM / LOW -- [reason]

**Verdict Definitions:**
- **PASS**: All criteria verified with evidence. Edge cases tested. No issues found.
- **FAIL**: One or more criteria failed. Issues listed with evidence.
- **PARTIAL**: Some criteria verified, others untested. No failures found but coverage incomplete.

Rules:
- Never issue PASS without running at least one command.
- Never issue PASS if any criterion is untested (that is PARTIAL).
- FAIL requires exact reproduction steps.
- If you cannot test something, say so.

#### Quick Verification (for small changes)

For Level 0 tasks (known patterns, less than 3 files):
1. State the one thing that must be TRUE.
2. Run one command that proves it.
3. Verdict: PASS or FAIL with evidence.

Skip the full protocol but never skip the evidence requirement.

---

### Integration Notes

- After writing code with TDD, immediately apply the Adversarial Verification protocol to the new implementation.
- During code review, use the protocol's mindset: try to break the code, don't just read it.
- When a user asks to "verify" or "check my work", default to the full verification process unless the change is trivial (then use Quick Verification).
- The debugging process's final "Confirm" step should include at least a Quick Verification to ensure the fix holds.
