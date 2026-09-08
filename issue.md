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

## 3. BUG: opening a non-existent file spams a persistent "file state" error

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
- **`pylsp` is not installed** on this machine (`my_editor --status` shows
  `✗ pylsp`, and `which pylsp` fails). The config points python's LSP at `pylsp`,
  so with it absent the server never really comes up — that is the primary reason
  Python "doesn't work". First step: install it
  (`pip install python-lsp-server`) and retest.
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
