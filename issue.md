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
