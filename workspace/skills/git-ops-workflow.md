---
name: git-ops-workflow
description: Git operations workflow — structured approach to git add, commit, push, pull with error handling and conflict resolution
triggers:
  - git
  - коммит
  - push
  - pull
  - merge
  - rebase
  - git операции
  - git workflow
tools_required:
  - code_exec
priority: 0
state: active
---

# Git Operations Workflow

## Core Principle
Always verify state before acting. Never force-push without explicit user confirmation.

## Workflow

### 1. Status Check
```
git status
git diff --stat
```
Determine: staged, unstaged, untracked files.

### 2. Stage Changes
- Review changes before staging
- Use `git add` for specific files, not `git add .` unless instructed
- For partial staging: `git add -p`

### 3. Commit
- Write descriptive commit messages in imperative mood
- Format: `<type>: <description>` (feat, fix, docs, refactor, chore)
- Include body for non-trivial changes

### 4. Sync
- **Pull before push**: `git pull --rebase` to avoid merge commits
- **Push**: `git push origin <branch>`
- On conflict: report exact files and conflict markers, do not auto-resolve

### 5. Error Handling
- **Permission denied**: report, suggest SSH key check
- **Diverged branches**: report divergence count, suggest rebase strategy
- **Merge conflicts**: list conflicted files, ask user for resolution strategy
- **Detached HEAD**: suggest creating a branch before committing

## Safety
- Never `git push --force` without user confirmation
- Never reset committed work without user confirmation
- Always show diff before committing