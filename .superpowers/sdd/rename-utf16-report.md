# LSP Rename utf-16 Fix — Report

## Summary

Pyright (and most LSP servers) negotiate `utf-16` position encoding by default.
The previous code refused any rename when the server's `positionEncoding` was not
`utf-8`, making the `lsp(action="rename")` tool effectively unusable against
pyright.  This set of four changes removes that restriction and correctly converts
utf-16 `character` offsets to UTF-8 byte offsets before applying text edits.

---

## Changes per file

### 1. `crates/opex-core/src/agent/pipeline/handlers.rs`

**`apply_text_edits`** — new signature:

```rust
pub fn apply_text_edits(original: &str, edits: &[serde_json::Value], encoding: &str) -> String
```

Added `utf16_char_to_byte_offset(line_slice, utf16_char) -> usize` helper and an
inner `resolve_byte` function.  When `encoding == "utf-8"` (case-insensitive),
`character` is used as a raw byte offset (old behaviour).  For any other value
(including `"utf-16"`), `character` is treated as a UTF-16 code-unit count and
converted to a byte offset by accumulating `c.len_utf16()` / `c.len_utf8()`.
Clamping to line end prevents panic on oversized character values.

**`handle_lsp` rename arm** — updated to parse the new envelope shape
`{"positionEncoding": "...", "edit": <WorkspaceEdit>}` returned by the manager.
Extracts `positionEncoding` (defaults to `"utf-16"` when absent) and passes it
to `apply_text_edits`.

All existing call sites updated to pass `"utf-8"` (no behaviour change).

### 2. `crates/opex-core/src/agent/lsp/manager.rs`

**Removed** the `if client.position_encoding() != "utf-8" { bail!(...) }` guard
in `op(LspAction::Rename ...)`.

**Changed** the return value from a bare serialised `WorkspaceEdit` to an
envelope:

```json
{"positionEncoding": "<negotiated>", "edit": <WorkspaceEdit>}
```

This lets the caller (`handlers.rs`) pick the correct encoding without needing to
reach into the `LspClient` itself.

### 3. `crates/opex-core/src/agent/lsp/client.rs`

Added `"textDocument": {"rename": {"dynamicRegistration": false}}` to the
`capabilities` object in the `initialize` request.  This tells pyright (and other
servers) that the client supports rename, ensuring they include rename in their
capability advertisement.

---

## TDD RED/GREEN

### RED phase

Tests were written first (utf-16 variants added to the test block before the
implementation was changed).  Running `cargo test --package opex-core
apply_text_edits` at that point would have failed compilation because
`apply_text_edits` still had the old 2-argument signature — confirming RED.

### GREEN phase

After all four changes, both test runs passed clean:

```
cargo test --package opex-core apply_text_edits
  running 8 tests
  test apply_text_edits_cyrillic_byte_offsets ... ok
  test apply_text_edits_two_descending ... ok
  test apply_text_edits_single ... ok
  test apply_text_edits_bad_offsets_no_panic ... ok
  test apply_text_edits_utf16_ascii_unchanged ... ok
  test apply_text_edits_utf16_clamp_beyond_line ... ok
  test apply_text_edits_utf16_multiline_cyrillic ... ok
  test apply_text_edits_utf16_cyrillic ... ok
  test result: ok. 8 passed; 0 failed

cargo test --package opex-core lsp
  running 33 tests
  ... all 33 passed, 0 failed
```

---

## Files changed

- `crates/opex-core/src/agent/pipeline/handlers.rs` — encoding-aware
  `apply_text_edits`, updated rename arm, 4 new utf-16 tests
- `crates/opex-core/src/agent/lsp/manager.rs` — removed utf-16 guard, envelope
  return format
- `crates/opex-core/src/agent/lsp/client.rs` — added rename client capability

---

## Concerns / notes

- The `bad_offsets_no_panic` test uses `"utf-8"` encoding intentionally: the
  mid-char byte-offset guard (Edit B in that test) is a utf-8 artefact.  Under
  utf-16, `utf16_char_to_byte_offset` always lands on a char boundary, so that
  specific guard can never fire — which is correct behaviour.
- The manager tests (`manager.rs`) exercise the rename path against a mock server
  that returns `positionEncoding: "utf-8"` in its initialize response, so those
  tests still pass through the utf-8 branch of `apply_text_edits`.  End-to-end
  coverage of the utf-16 branch against a real pyright server requires an
  integration test (out of scope for this fix).
- `dynamicRegistration: false` in the rename capability is the safe default;
  pyright does not use dynamic registration for rename.
