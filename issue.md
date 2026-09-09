# Pending decisions

Refactoring / hardening notes that need a human call. Each item says what was
found, the options, and a recommendation. Nothing here has been acted on beyond
what the linked commit states.

## 1. Wire `cargo deny` into an automated gate?

**Status:** `deny.toml` is now thorough (graph / advisories / bans / licenses /
sources) and `cargo deny check` passes clean (exit 0, no warnings). But it is
**not run automatically** — the git hooks only run `cargo fmt`, `cargo clippy`,
and `cargo test`. So a newly introduced vulnerable/unmaintained crate, a
disallowed license, or a non-crates.io source would not be caught until someone
runs `cargo deny` by hand.

**Options:**
- (a) Add `cargo deny check` to `.githooks/pre-push` (alongside `cargo test`).
  Pro: enforced before publishing. Con: requires `cargo-deny` installed on the
  machine (like cargo itself); slows push; needs network for the advisory DB
  unless `--offline`/cached.
- (b) Leave it manual / to a future CI. Pro: no local friction. Con: not enforced.

**Recommendation:** (a) in pre-push, but only once you are comfortable
`cargo-deny` is reliably installed in your environments. Low urgency.

## 2. License allow-list is trimmed to what is currently used

`deny.toml` allows only the licenses present in the tree today (MIT, Apache-2.0,
Apache-2.0 WITH LLVM-exception, Unicode-3.0, Zlib, BSL-1.0). This is stricter than
my_shell's broader list: a new dependency with a different (even permissive)
license will **fail** `cargo deny check licenses` until you add it deliberately.
That is the intended review gate, but if you would rather not be interrupted,
widen the list to the common-permissive set instead. No action needed unless the
friction bites.

## 3. BUG: opening a non-existent file spams a persistent "file state" error — RESOLVED

**Resolution:** `disk_state` now maps `ErrorKind::NotFound` to `Ok(None)` ("no
file on disk yet") instead of an `Err` string, and the `DiskStateObserved`
observer treats `Ok(None)` as "not on disk yet": it clears `disk_state` and the
external-change flag without touching the status line. Covered by
`observing_a_missing_file_is_not_an_error_and_clears_external_change`. The
original report is kept below for context.

**Symptom:** Opening a path that does not exist (e.g. `bash_practice/d.txt`)
leaves the status line stuck showing
`ファイル状態を取得できません <path>: No such file or directory (os error 2)`,
and saving does not clear it.

**Cause:** `disk_state()` (src/runtime/mod.rs:1188) does `fs::metadata` and returns
an `Err` string when the file is missing. That error is surfaced by the `Err`
branch of `IoEvent::DiskStateObserved` (src/editor/mod.rs:1158) which sets
`self.status`. The disk-state check re-runs periodically (`Effect::CheckDiskStates`),
so for a document whose file does not exist yet the error is re-shown on every
tick — hence "persistent". Opening a not-yet-created file is a normal workflow
(you open it to create it), so a missing file should be treated as "no file on
disk yet" (disk_state = None / not-modified externally), not an error.

**Suggested fix (not yet done — mid-refactor):** in `disk_state`, map
`ErrorKind::NotFound` to `Ok(None)`-style "no disk state" rather than `Err`, and
have the observer treat that as "file not on disk yet" (clear any external-change
flag, don't touch the status line). Confirm save then creates it and the state
settles. Needs a test: open missing path → no error status; save → file created,
status clean.

## 4. BUG: Python LSP appears not to work; an empty popup box shows

**Symptom:** Editing a `.py` file, the Python language features don't work
(no useful completion/diagnostics), the status line reads `<lsp> python:
coloring`, and an empty popup rectangle is drawn in the top-right.

**Findings:**
- `pylsp` **is now installed** (`my_editor --status` shows
  `✓ pylsp /home/shogo/.local/bin/pylsp`; clangd is present too). So a missing
  tool is *no longer* the explanation — the Python trouble is a real bug to
  investigate now that the server is available. Retest and capture what pylsp
  actually returns (completion/hover/diagnostics) vs. what the editor shows.
- Independent of that, **an empty popup is being rendered** (the top-right box
  with no content). A completion/hover/signature popup with nothing in it should
  not be shown. Worth confirming the guards: completion is gated on
  `!items.is_empty()` at src/editor/mod.rs:1397/1805, but check `completion_view`
  (mod.rs:904), `hover_view` (932), and `signature_help_view` (936) plus their
  render sites (render/mod.rs draw_completion:541, signature_help:180) for a path
  that yields an empty-but-Some view.
- The `<lsp> python: coloring` status is suspicious: python has no tree-sitter
  grammar wired (only json/toml/markdown/rust/bash/csh), so "coloring" may be a
  stale/misleading LSP phase label when the server failed to start.

**Suggested direction (not yet done):** (1) surface a clear "pylsp not found"
state the way shellcheck silently no-ops rather than showing a half-alive LSP;
(2) never render an empty popup; (3) reconsider the status label when the server
did not start. Revisit after installing pylsp to separate "tool missing" from
real bugs.

## 5. Refactor roadmap for editor/mod.rs (module placement)

Done so far (safe, behavior-preserving, each verified + committed):
- Tests → `editor/tests.rs`.
- Jump history → `editor/navigate.rs`.
- Snippet filling → `editor/snippet_session.rs`.
- Diff view → `editor/diff_view.rs`.

These were clean because they own no shared types (or none at all) and their
methods are cohesive. mod.rs went 10029 → ~6.1k lines.

**Blocker for the rest.** The remaining concerns (completion, lsp, search,
picker, shell/terminal, notifications) cannot be split cleanly yet because of two
coupling knots:

1. **God dispatch methods.** `update`, `apply_command`, `apply_lsp`, and
   `confirm_picker` each handle many concerns in one body. For example
   `apply_lsp` builds completion candidates, and `confirm_picker` applies a chosen
   completion, a picked file, a search hit, and a rename — so completion logic
   lives in three places at once. Until each of these is decomposed (one arm →
   one method per concern), the concern's code can't all move to one module.
2. **Shared types with private fields.** `CompletionCandidate`/`CompletionState`
   are constructed in `apply_lsp` (mod.rs) and consumed in `confirm_picker`
   (mod.rs) as well as in the completion methods; `SearchState` is stored inside
   `layout::RightPane::Search`, so it is shared with the layout module. Moving
   such a type to a concern submodule would force most of its fields to
   `pub(super)`, which *raises* coupling rather than lowering it — the opposite of
   the goal.

**Recommended order (needs your OK — it is a bigger change than the moves above):**
1. Decompose the dispatch god-methods: give `apply_lsp` one handler per response
   kind, and split `confirm_picker` into `confirm_completion` / `confirm_search` /
   `confirm_rename` / `confirm_file`. Behavior-preserving, each verified.
2. Once completion logic is in dedicated methods, move them + the completion types
   into `editor/completion.rs` (types now have a single owning module).
3. Repeat for lsp (`editor/lsp.rs`: LspServer, PendingLsp, SignatureHelp*, the
   request/response methods, the hover/signature free fns), search
   (`editor/search.rs`; decide whether `SearchState` stays with layout or the
   variant becomes a thin handle), picker, and shell/terminal.
4. Group the remaining pure free functions with their concern modules.

This is the "where should each type/function live" design. Step 1 is the
prerequisite and the main decision: it is a real (if mechanical) change to central
control flow, so it is left here rather than done unprompted.
