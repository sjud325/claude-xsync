# claude-xsync

Cross-platform sync for your `~/.claude` state (Claude Code sessions, settings,
agents/skills, MCP config, memory) between **macOS and native Windows** — with
different usernames, different path separators, and different drive layouts —
over a **private, end-to-end-encrypted git repository**.

Two machines behave like one machine: push on the Mac, pull on the Windows box,
run `claude --resume`, and your session continues with every path rewritten to
the local home.

## Why another sync tool?

Cross-OS **path correctness** is the whole point. Existing tools get close but
break exactly there (state of 2026-07-14):

- **[tawanorg/claude-sync](https://github.com/tawanorg/claude-sync)** (Go, MIT) —
  the most mature prior art, actively released. However (1) its GitHub Releases
  have been missing the Windows binaries for ~2 months due to an incomplete
  `.releaserc.json` asset list (issue #31 open; a win package is being attempted
  via npm instead), and (2) its path normalization
  (`internal/sync/paths.go`) substitutes the home-prefix bytes only: it does not
  convert separators (`\` ↔ `/`) and does not match JSON-escaped `C:\\Users\\…`
  forms, so Mac↔Windows content paths break. Its content-transform tests are
  Unix-path centric.
- **[claude-context-sync](https://github.com/dbtlr/claude-context-sync)**
  (Python, newer) — advertises cross-drive template variables, but its
  `denormalize` (`src/path_transformer.py`) forces `\\` separators on every
  output, breaking Mac targets. No LICENSE file at the time of review.

claude-xsync exists to make the path problem *impossible by construction*:

- **Span-splicing transform**: a JSON string-span lexer rewrites only the string
  bytes that actually contain this device's home paths. Everything else is
  byte-preserved — no re-serialization damage (float re-formatting, key
  reordering) can occur, ever.
- **Verified round trips**: every push is gated by a byte-exact round-trip check
  (invariant A) and a pull fixpoint check (invariant B). A file that cannot be
  proven safe is stored *verbatim* instead — corrupted paths can never reach the
  remote. Pulls reverse-verify before touching `~/.claude`.
- **Injective token encoding**: literal `${HOME}` text in your sessions (shell
  discussions!) is escaped before tokens are inserted, so inserted tokens and
  quoted literals can never be confused.
- **Case/separator/escape-form tolerant**: `C:\Users\Loki`, `C:/Users/Loki`,
  `c:\users\loki`, and their JSON-escaped forms all match; output uses the
  canonical case, and Windows restores as forward-slash form (`C:/Users/…`).

## Security model

Everything is compressed (gzip) then encrypted (age) **before** it touches git.
The remote sees: object count, sizes, commit times, and a random salt — never
content, filenames, or project names (object names are HMAC-keyed hashes).
Passphrase → Argon2id (m=64 MiB, t=3, p=1) → x25519 key + HMAC key.

**Losing the passphrase makes the remote unrecoverable** (by design). Your
plaintext still lives on each device; re-`init` to recover. Rotate with
`claude-xsync rekey`, which re-encrypts everything and squashes history so
nothing decryptable with the old key survives.

## Quickstart

Create a **private** GitHub repo, then on the first device:

```bash
export XSYNC_PASSPHRASE='your-long-passphrase'
claude-xsync init --remote git@github.com:you/claude-state.git --device mac
claude-xsync push
```

On the second device (same passphrase):

```bash
set XSYNC_PASSPHRASE=your-long-passphrase   # PowerShell: $env:XSYNC_PASSPHRASE='…'
claude-xsync init --remote git@github.com:you/claude-state.git --device win
claude-xsync pull
```

Recommended: add a note to your synced `~/.claude/CLAUDE.md` so the model
adapts to whichever machine you're on:

```
This ~/.claude is synced between macOS (/Users/<mac-user>) and Windows
(C:\Users\<win-user>). Path mentions in older turns may reference the
other machine; trust the current pwd/environment.
```

Daily flow: finish work → `push`; sit down at the other machine → `pull`.
Both commands support `--dry-run`, refuse to run while Claude Code is open
(`--force` to override), and print a fixed summary:
`✓ N synced · ⚠ M verbatim (reason) · ✗ K skipped · ⚡ C conflicts`.

Other commands:

- `claude-xsync status` — to-push / to-pull / conflict counts (`--offline` skips fetch)
- `claude-xsync gc --squash` — rewrite remote history into one commit
  (recommended monthly or at ~1 GB; encrypted blobs don't delta, so an active
  large session can add ~30 MB of history per push). The other device
  auto-recovers on its next pull.
- `claude-xsync rekey` — new passphrase + salt, full re-encrypt, mandatory squash

If you forget to push and edit on both machines, pull classifies per file:
local-only work is preserved, remote-only changes apply, true conflicts keep your
copy as `<file>.xsync-conflict.<ts>`, and `history.jsonl` is merged by line
union (append-only, so nothing is lost). Replaced/deleted files are first
copied to `~/.claude.backup.<ts>/`. First pull on a device with pre-existing
data: same mechanism — backups + conflict copies (use `--dry-run` to preview).

## What is synced

Allowlist: `projects/`, `history.jsonl`, `file-history/`, `tasks/`, `todos/`,
`plans/`, `settings.json`, `settings.local.json`, `CLAUDE.md`,
`keybindings.json`, `agents/`, `skills/`, `commands/`, `rules/`, `workflows/`,
plugin manifest files, and the `mcpServers` subtree of `~/.claude.json` (merged
key-only on pull — machine identity keys are never transferred). Machine state
(`ide/`, `sessions/`, caches, `.credentials.json`, …) is permanently excluded. New
unknown top-level entries are reported, never silently synced.

## Known limitations

- **Paths containing spaces** inside file *content* end the match at the space.
  The round-trip gate catches any damage and stores the file verbatim — nothing
  corrupts, but such paths aren't rewritten. Prefer space-free project paths.
- **Quoted peer-home text is absorbed once**: each device rewrites only its
  *own* home forms, so a peer-home path quoted in conversation text arrives
  intact on the peer, then becomes a live (translating) path from the next
  push on (reported as `absorbed` during pull). Machine-consumed paths (cwd,
  checkpoint keys) are unaffected throughout. Case/separator variants of the
  quote absorb too and canonicalize over subsequent hops; genuinely damaged
  content still fails closed (skipped). If you need permanent quote fidelity,
  that is the v2 peer-home registry (spec §11).
- **Left-boundary residual**: paths under a different root that embed a home
  shape (e.g. `/mnt/backup/Users/<name>/…`) still translate, because a
  preceding separator is allowed — that is what keeps `file:///…` and `\\?\…`
  prefixes translating. Concatenated forms after a non-separator character
  (like the `/System/Volumes/Data/Users/<name>` firmlink alias) are preserved
  as-is.
- **Directory-key encoding is lossy**: using a sibling of your home
  (`/Users/woong.bak/…`) as a project root can mis-tokenize its `projects/`
  key. Rare; avoid that layout.
- **MCP/permissions gap**: only the global `mcpServers` subtree syncs.
  Per-project enablement, `allowedTools`, and trust state rebuild per device
  (v2 candidate). MCP servers whose `command` is an OS-specific absolute path
  (`/opt/homebrew/…`) can't be fixed by path rewriting — use `path_map` in
  `config.toml` or per-OS overrides.
- **Hooks / statusLine commands** in settings sync as-is; the executable and
  shell syntax must exist on both OSes (paths are rewritten, semantics aren't).
- **Metadata visibility**: the remote exposes object count/sizes/commit times
  and the salt. Content, names, and paths are never visible. There is no
  freshness protection: whoever can force-push the remote can replay an older
  (internally consistent) snapshot; devices would realign to it, keeping
  local backups as the only trace.
- **Backups accumulate**: every pull that replaces files writes a full copy
  under `~/.claude.backup.<ts>/` and nothing prunes them — clean up
  periodically if disk space matters.
- **Korean filenames (NFC/NFD)**: macOS decomposes filenames (NFD) while
  Windows keeps NFC; if you hit duplicate-looking files, normalize project
  filenames to NFC.

## Real-machine validation status

> **UNVERIFIED — must be executed on the actual Windows (Loki) machine before
> calling v1 done.** This environment cannot run it.

- ☐ Windows slash-cwd `claude --resume` works on a pulled session
- ☐ checkpoint/rewind works when `trackedFileBackups` keys are slash-form
- ☐ plugins reinstall after pull
- ☐ identical behavior from Git Bash and PowerShell (`init`/`push`/`pull`)
- ☐ `.claude.json` mcp merge leaves login intact
- ☐ Windows reserved-name file inside `skills/` → pull skips + reports
- ☐ MAX_PATH-exceeding path handled via `\\?\`
- If slash-cwd resume fails → implement spec §5 plan B (JSON-string-scoped
  backslash re-escaping in pull-resolve; the tokenizer already provides the
  machinery) and re-run this checklist.

The Windows golden fixture is currently synthetic
(`tests/fixtures/win-session.jsonl`, marked `synthetic-until-real-capture`);
replace it with a masked real capture from the Windows machine via
`scripts/mask-fixture.py`.

## Releasing (maintainer notes)

Releases are built by `.github/workflows/release.yml` for 6 targets
(darwin arm64/x64, windows x64/arm64, linux x64/arm64) and the workflow
**fails if the release has fewer than 7 assets** (6 binaries + checksums.txt) —
a structural guard against the silently-missing-binary failure mode.

To cut a release (manual step, not automated by tooling):

```bash
git remote add origin git@github.com:<you>/claude-xsync.git   # once
git push -u origin main
git tag v0.1.0-alpha && git push origin v0.1.0-alpha
# then watch the Release workflow produce 6 binaries + checksums.txt
```

Also installable with `cargo install --path .`.

## Credits

- [tawanorg/claude-sync](https://github.com/tawanorg/claude-sync) — prior art
  for the overall push/pull-over-git shape.
- [claude-context-sync](https://github.com/dbtlr/claude-context-sync) — prior
  art for path template variables.

## License

MIT — see [LICENSE](LICENSE).
