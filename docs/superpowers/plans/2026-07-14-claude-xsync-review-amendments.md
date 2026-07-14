# claude-xsync Review Amendments (spec §12) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use markdown checkbox syntax for tracking.

**Goal:** Implement spec §12 (post-adversarial-review amendments): separator-exception left boundary for path matching (§12.1) and one-step-stability relaxation of the pull gate (§12.2, decision C′).

**Architecture:** Both changes are narrow: §12.1 adds a left-boundary predicate to the `normalize_text` scan loop in the pure transform core; §12.2 adds a fallback check in `stage_entry` (pull staging) that accepts a reverse-verify mismatch only when the re-tokenized form is provably a fixpoint. Existing e2e regression `pull_skip_must_not_cascade_into_push_clobber` must keep its purpose (skip → no clobber) by switching its trigger to a *non-canonical-case* quoted home, which C′ still rejects (unstable).

**Tech Stack:** Rust 2021, existing crate — no new dependencies.

## Global Constraints

- All prior plan constraints hold (env overrides for tests, TDD, conventional commits, never weaken a failing test).
- Left boundary rule (spec §12.1 verbatim): 매칭 시작 직전 문자가 경로 런 문자이면 불일치. 단 구분자(`/`, `\`)는 예외로 허용.
- C′ rule (spec §12.2 verbatim): 역검증 실패 시 `d2 = normalize(resolved)`에 대해 `resolve_pull(d2) == resolved`이면 적용(+알림), 아니면 스킵.
- `cargo test --all-targets`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` must stay green.

---

### Task 1: Left boundary with separator exception (§12.1)

**Files:**
- Modify: `src/transform/pathmatch.rs` (normalize_text loop + tests)
- Check: `tests/fixtures/*.portable.jsonl` goldens (should be unchanged; if a golden changes, inspect the diff — it means a fixture contained a concatenated form — and regenerate with `UPDATE_GOLDEN=1` only if the new output is correct)

**Interfaces:**
- Consumes: `is_run_char` (private, already in pathmatch.rs).
- Produces: no signature changes — `normalize_text` behavior narrows (fewer matches). Invariants A/B are preserved automatically because verify/pull resolve consume whatever normalize produced.

- [x] **Step 1: Write the failing tests** (append to `#[cfg(test)] mod tests` in pathmatch.rs)

```rust
#[test]
fn left_boundary_blocks_concatenated_home() {
    // macOS firmlink alias: previous char 'a' is a run char → no match
    let n = normalize_text("/System/Volumes/Data/Users/woong/ws", &mac());
    assert_eq!(n.text, "/System/Volumes/Data/Users/woong/ws");
    assert!(n.spans.is_empty());
}

#[test]
fn left_boundary_allows_separator_prefix_urls() {
    // file:// URLs keep translating (previous char '/')
    let n = normalize_text("open file:///Users/woong/doc.md now", &mac());
    assert_eq!(n.text, "open file://${HOME}/doc.md now");
}

#[test]
fn left_boundary_allows_longpath_prefix() {
    // \\?\ long-path prefix keeps translating (previous char '\')
    let n = normalize_text(r"\\?\C:\Users\Loki\ws", &win());
    assert_eq!(n.text, r"\\?\${HOME}/ws");
}

#[test]
fn left_boundary_blocks_alnum_prefix_win() {
    let n = normalize_text(r"xC:\Users\Loki\ws", &win());
    assert_eq!(n.text, r"xC:\Users\Loki\ws");
    assert!(n.spans.is_empty());
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test pathmatch::tests::left_boundary`
Expected: FAIL — `left_boundary_blocks_concatenated_home` and `left_boundary_blocks_alnum_prefix_win` assert untouched text but current code tokenizes.

- [x] **Step 3: Implement** — track the previous char in the normalize scan loop

In `normalize_text`, replace the scan loop with:

```rust
    let mut out = String::with_capacity(escaped.len());
    let mut spans = Vec::new();
    let mut i = 0;
    let mut prev: Option<char> = None;
    'outer: while i < escaped.len() {
        // Left boundary (spec §12.1): a run char directly before the match
        // start blocks it — except separators ('/', '\\'), which keep
        // file:/// URLs and \\?\ long-path prefixes translating.
        let left_ok = prev.map_or(true, |c| !is_run_char(c) || matches!(c, '/' | '\\'));
        if left_ok {
            for t in &m.tokens {
                if let Some(len) = match_local_at(&escaped, i, &t.local) {
                    let run_start = i + len;
                    let mut run_end = run_start;
                    for c in escaped[run_start..].chars() {
                        if is_run_char(c) { run_end += c.len_utf8(); } else { break; }
                    }
                    let original = escaped[i..run_end].to_string();
                    out.push_str(&format!("${{{}}}", t.name));
                    out.push_str(&escaped[run_start..run_end].replace('\\', "/"));
                    spans.push(SpanRecord { original });
                    prev = escaped[i..run_end].chars().last();
                    i = run_end;
                    continue 'outer;
                }
            }
        }
        let c = escaped[i..].chars().next().unwrap();
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
```

(Everything else in the function unchanged. `resolve_text` needs no change: it replays tokens normalize inserted.)

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: all pass, including the existing invariant proptest and goldens. If a golden fails, inspect the diff per Files note above.

- [x] **Step 5: Commit**

```bash
git add src/transform/pathmatch.rs
git commit -m "feat: separator-exception left boundary for home matching (spec 12.1)"
```

---

### Task 2: One-step-stability pull gate (C′, §12.2) + regression rework

**Files:**
- Modify: `src/cli/pull.rs` (`stage_entry`)
- Modify: `tests/e2e.rs` (rework `pull_skip_must_not_cascade_into_push_clobber` trigger; add `quoted_peer_home_absorbed_once_and_syncs`)
- Modify: `README.md` (limitations + CLAUDE.md snippet)

**Interfaces:**
- Consumes: `normalize_file`, `resolve_file_pull` (transform::file), existing `stage_entry` signature — unchanged.
- Produces: `stage_entry` now returns Ok(resolved) for the stable-absorption case and prints a `⚠ … absorbed` notice; unstable mismatches still Err → skip.

- [ ] **Step 1: Rework the C1 regression trigger** (it currently relies on quoted-home = skip, which C′ changes)

In `pull_skip_must_not_cascade_into_push_clobber`, make the quote non-canonical-case so C′ still rejects it (absorbing it would rewrite the quote's case — unstable, fail-closed):

```rust
    // A rewrites the file quoting B's home in NON-CANONICAL case — C′ still
    // skips this (re-tokenizing can't reproduce the original case), which is
    // exactly the skip we need to exercise the push guard.
    let v2 = format!(
        "{{\"note\":\"peer log said {}/x\"}}\n{{\"note\":\"IMPORTANT-V2\"}}\n",
        env.dev_b.home_str().to_uppercase()
    );
```

(Only the `v2` construction changes; every assertion in the test stays as is.)

- [ ] **Step 2: Write the failing absorption e2e test** (append to tests/e2e.rs)

```rust
#[test]
fn quoted_peer_home_absorbed_once_and_syncs() {
    // Spec §12.2 (C′): a session quoting the peer's home (canonical case)
    // syncs to that peer; the quote is absorbed as a live path exactly once,
    // after which everything is a stable fixpoint.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let rel = "plans/quoted.jsonl";
    let original = format!(
        "{{\"note\":\"peer log said {}/x\"}}\n{{\"note\":\"DATA-KEEP\"}}\n",
        env.dev_b.home_str()
    );
    fs::create_dir_all(env.dev_a.claude().join("plans")).unwrap();
    fs::write(env.dev_a.claude().join(rel), original.as_bytes()).unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // B pull: file APPLIES (no skip) with the quote byte-intact, plus notice
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "pull must apply the quoted file: {o}");
    assert!(o.contains("absorbed"), "expected absorption notice: {o}");
    assert_eq!(
        fs::read_to_string(env.dev_b.claude().join(rel)).unwrap(),
        original,
        "quote must arrive byte-intact on first hop"
    );

    // B push: re-normalization tokenizes the absorbed quote and converges
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // A pull: the quote has morphed ONCE into A's live home; data intact
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let after = fs::read_to_string(env.dev_a.claude().join(rel)).unwrap();
    assert!(after.contains(&format!("{}/x", env.dev_a.home_str())), "morph: {after}");
    assert!(after.contains("DATA-KEEP"), "{after}");

    // converged: both sides now no-op
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("✓ 0 synced"), "must be a fixpoint now: {o}");
}
```

- [ ] **Step 3: Run tests to verify the new one fails and the reworked one still fails-for-the-right-reason**

Run: `cargo test --test e2e quoted_peer_home pull_skip`
Expected: `quoted_peer_home_absorbed_once_and_syncs` FAILS (pull exits 1, file skipped). `pull_skip_must_not_cascade_into_push_clobber` PASSES already (uppercase quote still skips under current code) — that is fine; it is a guard-rail for Step 4.

- [ ] **Step 4: Implement C′ in `stage_entry`**

Replace the `EntryMode::Transformed` arm:

```rust
        EntryMode::Transformed => {
            let resolved =
                resolve_file_pull(portable, &payload, mapper).map_err(|err| anyhow::anyhow!("{err}"))?;
            match normalize_file(portable, &resolved, mapper) {
                TransformOutcome::Transformed { data, .. } if data == payload => Ok(resolved),
                TransformOutcome::Transformed { data: d2, .. } => {
                    // One-step stability (spec §12.2, decision C′): quoted
                    // peer-home text re-tokenizes under THIS device's mapper.
                    // That is absorption, not corruption — but only if the
                    // absorbed form is already a fixpoint; anything else is
                    // treated as damage and skipped.
                    let stable = resolve_file_pull(portable, &d2, mapper)
                        .map(|r2| r2 == resolved)
                        .unwrap_or(false);
                    if stable {
                        println!(
                            "⚠ {portable}: quoted peer-device path text absorbed as live local paths (one-time; see README)"
                        );
                        Ok(resolved)
                    } else {
                        anyhow::bail!("pull reverse-verify failed (resolved form does not re-normalize)")
                    }
                }
                _ => anyhow::bail!("pull reverse-verify failed (resolved form does not re-normalize)"),
            }
        }
```

- [ ] **Step 5: Run the full suite**

Run: `cargo test --all-targets`
Expected: all pass — the absorption test goes green; `pull_skip_must_not_cascade_into_push_clobber` still passes because the uppercase quote fails the stability check (case is not reproducible from the canonical home).

- [ ] **Step 6: Update README**

Replace the "Quoted other-OS paths…" limitation bullet with the one-time-absorption semantics, drop the now-fixed mid-string bullet in favor of the residual `…/Users/<name>`-under-other-root caveat, and add a recommended shared-CLAUDE.md snippet to the quickstart:

```markdown
- **Quoted peer-home text is absorbed once**: each device rewrites only its
  *own* home forms, so a peer-home path quoted in conversation text arrives
  intact on the peer, then becomes a live (translating) path from the next
  push on (reported as `absorbed` during pull). Machine-consumed paths are
  unaffected. Non-canonical-case quotes that can't absorb losslessly are
  skipped instead (fail-closed). If you need permanent quote fidelity, that
  is the v2 peer-home registry.
- **Left-boundary residual**: paths under a different root that embed a home
  shape (e.g. `/mnt/backup/Users/<name>/…`) still translate, because the
  preceding separator is allowed (this is what keeps `file:///…` and `\\?\…`
  translating). Avoid that layout.
```

Quickstart addition:

```markdown
Recommended: add a note to your synced `~/.claude/CLAUDE.md` so the model
adapts to whichever machine you're on:

    This ~/.claude is synced between macOS (/Users/<mac-user>) and Windows
    (C:\Users\<win-user>). Path mentions in older turns may reference the
    other machine; trust the current pwd/environment.
```

- [ ] **Step 7: Verify + Commit**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets
git add src/cli/pull.rs tests/e2e.rs README.md
git commit -m "feat: one-step-stability pull gate absorbs quoted peer-home text (spec 12.2)"
```

---

## Self-review notes

- Spec coverage: §12.1 → Task 1; §12.2 (gate rule, absorption semantics, README/CLAUDE.md guidance) → Task 2. §12.2's "알림" is the `absorbed` println asserted in the e2e test.
- Type consistency: no public signatures change; `stage_entry` still `-> anyhow::Result<Vec<u8>>`.
- The reworked C1 trigger (non-canonical-case quote) is deliberate: C′ must reject it (stability check fails on case), so the push-guard regression keeps exercising a genuine skip.
