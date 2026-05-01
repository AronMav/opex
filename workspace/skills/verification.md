---
name: verification
description: Adversarial verification protocol -- try to break it, not confirm it works
triggers:
  - verify
  - check my work
  - is this correct
  - test this
  - validation
  - проверь
  - верификация
  - протестируй
priority: 9
state: active
---

## Adversarial Verification Protocol

### Core Principle

Your job is to try to BREAK it, not confirm it works. Approach every verification as a hostile reviewer. If you find yourself thinking "this looks right" -- you are doing it wrong. Find the evidence or declare it unverified.

### Anti-Rationalization Warnings

STOP if you catch yourself:

- "This probably works because..." -- probably is not evidence
- "The code looks correct" -- reading is not testing
- "It should handle that case" -- should is not does
- "I already checked this" -- show the output or check again
- Skipping edge cases because the happy path worked
- Declaring PASS without running a single command

The goal is a verdict backed by exact evidence, not a feeling of confidence.

### Verification Process

#### Step 1: Define Success Criteria

Before touching anything, write down what must be TRUE for this to pass. Use concrete, observable statements:

- BAD: "The API works correctly"
- GOOD: "POST /api/items returns 201 with {id, name, createdAt} when given valid {name} body"
- GOOD: "Invalid email input shows red border and 'Invalid email' text below the field"

Each criterion needs:

- **Input** -- what you provide
- **Expected output** -- what you observe
- **How to check** -- the exact command, URL, or action

#### Step 2: Verify the Happy Path

Run the expected scenario. Capture exact output in this format:

```
EVIDENCE: [paste exact command and output]
CRITERION: [which criterion this satisfies]
RESULT: PASS / FAIL
```

Do NOT proceed to edge cases if the happy path fails. Fix first.

#### Step 3: Try to Break It

For each verified criterion, attempt to violate it:

**Input attacks:**
- Empty input, null, undefined
- Extremely long input (10K+ characters)
- Special characters: script tags, SQL injection, path traversal
- Wrong types: string where number expected, array where object expected
- Boundary values: 0, -1, MAX_INT, empty array

**State attacks:**
- Call without authentication
- Call with expired/invalid token
- Call twice rapidly (race condition)
- Call with stale data
- Call after the resource was deleted

**Environment attacks:**
- Network timeout (does it hang forever?)
- Database unavailable (does it crash or return error?)
- File not found (does it create or fail gracefully?)
- Permission denied (does it surface the error?)

#### Step 4: Check What Was NOT Tested

List every criterion from Step 1. Mark each:

- **TESTED** -- with evidence reference
- **NOT TESTED** -- with reason

Untested criteria are PARTIAL, not PASS.

#### Step 5: Render Verdict

Use this format:

```
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
```

### Verdict Definitions

- **PASS**: All criteria verified with evidence. Edge cases tested. No issues found.
- **FAIL**: One or more criteria failed. Issues listed with evidence.
- **PARTIAL**: Some criteria verified, others untested. No failures found but coverage incomplete.

Rules:

- Never issue PASS without running at least one command.
- Never issue PASS if any criterion is untested (that is PARTIAL).
- FAIL requires exact reproduction steps.
- If you cannot test something, say so.

### Quick Verification (for small changes)

For Level 0 tasks (known patterns, less than 3 files):

1. State the one thing that must be TRUE.
2. Run one command that proves it.
3. Verdict: PASS or FAIL with evidence.

Skip the full protocol but never skip the evidence requirement.
