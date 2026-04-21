# Palette — UX Discovery Journal

## 2026-04-21 - `title` vs `aria-label` on icon-only buttons

**Discovery:** The codebase uses `title` for tooltips on icon-only buttons (e.g., the export Download button), but `title` is not reliably announced by screen readers. `aria-label` is the correct attribute for accessible names. Several icon-only X (close/remove) buttons had neither.

**Action:** Always use `aria-label` on icon-only buttons. `title` is fine to keep alongside for mouse hover tooltip, but never rely on it alone for accessibility.
