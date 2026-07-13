# claude-xsync v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cross-platform (macOS ↔ native Windows) Claude Code state sync CLI over an encrypted private git repo, with span-splicing path normalization that survives username/OS/separator/case differences.

**Architecture:** Pure transform core (JSON string-span lexer + path matcher with shape records) isolated from thin I/O layers (git shell-out, age crypto, atomic fs). Push = scan→normalize→verify→seal→manifest→git. Pull = git→manifest→classify(5-way)→resolve→reverse-verify→atomic apply. Spec: `docs/superpowers/specs/2026-07-14-claude-xsync-design.md` (v0.2 — read it first; this plan implements it exactly).

**Tech Stack:** Rust 2021. Deps: `clap`(derive), `serde`+`serde_json`, `age`, `flate2`, `sha2`, `hmac`, `argon2`, `dirs`, `thiserror`, `anyhow`, `tempfile`, `toml`, `hex`, `sysinfo`. Dev: `proptest`. Forbidden: tokio, git2, any cloud SDK.

## Global Constraints

- Portable form: separators always `/`; tokens `${NAME}`; reserved token names: `HOME`, `ESC` (mapper must reject these in path_map).
- Injectivity escape: normalize rewrites every literal `${` → `${ESC}` BEFORE token insertion; pull/verify resolve replaces registered tokens FIRST, then `${ESC}` → `${`. Order is mandatory.
- Home matching: current device's home forms only; case-insensitive matching; canonical-case output from `dirs::home_dir()`.
- Path-run charset (after matched home prefix): `c.is_alphanumeric() || matches!(c, '_'|'-'|'.'|'/'|'\\'|'~'|'+'|'@') || !c.is_ascii()`. No spaces.
- Verify gate = invariant A (byte round-trip via recorded span originals) && invariant B (fixpoint: `normalize(resolve_pull(n)) == n`). Failure ⇒ Verbatim, never error.
- Windows pull-resolve emits forward-slash form (`C:/Users/Loki/...`).
- Argon2id params (fixed constants): m=65536 KiB, t=3, p=1, output 32 bytes. Salt: 32 random bytes stored plaintext at repo root file `salt`.
- Object names: lowercase hex `HMAC-SHA256(derived_key_bytes, portable_path)`. Chunk at 90 MiB (`.age.0`, `.age.1`, …).
- Exit codes: 0 clean / 1 completed-with-warnings / 2 aborted-nothing-done.
- Env overrides for tests: `XSYNC_CLAUDE_DIR` (default `~/.claude`), `XSYNC_DIR` (default `~/.claude-xsync`), `XSYNC_HOME` (default `dirs::home_dir()`).
- Commits: conventional (`feat:`, `test:`, `chore:`); one commit per green TDD cycle.
- Every module in `src/` except `cli/` and `main.rs` must compile without touching the real filesystem in unit tests.

---

### Task 1: Scaffold + PathMapper

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/lib.rs`, `src/mapper.rs`
- Test: inline `#[cfg(test)]` in `src/mapper.rs`

**Interfaces:**
- Produces: `PathMapper::new(home: &str, path_map: &BTreeMap<String,String>) -> Result<PathMapper, MapperError>`, `PathMapper { pub tokens: Vec<TokenMapping> }`, `TokenMapping { pub name: String, pub local: String, pub enc_local: String }`. Tokens sorted longest-`local`-first; `HOME` always present; `local` stored trailing-separator-trimmed, canonical case.

- [x] **Step 1: Scaffold**

```bash
cd ~/workspace/claude-xsync && cargo init --name claude-xsync
```

`Cargo.toml`:
```toml
[package]
name = "claude-xsync"
version = "0.1.0"
edition = "2021"
license = "MIT"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
age = "0.10"
flate2 = "1"
sha2 = "0.10"
hmac = "0.12"
argon2 = "0.5"
dirs = "5"
thiserror = "1"
anyhow = "1"
tempfile = "3"
toml = "0.8"
hex = "0.4"
sysinfo = "0.30"

[dev-dependencies]
proptest = "1"
```

`src/lib.rs`:
```rust
pub mod mapper;
```
`src/main.rs`:
```rust
fn main() { println!("claude-xsync"); }
```

- [x] **Step 2: Write failing tests** (in `src/mapper.rs` bottom)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn home_always_mapped_and_longest_first() {
        let mut pm = BTreeMap::new();
        pm.insert("/Users/woong/work".into(), "WORK".into());
        let m = PathMapper::new("/Users/woong", &pm).unwrap();
        assert_eq!(m.tokens[0].name, "WORK"); // longer local first
        assert_eq!(m.tokens[1].name, "HOME");
        assert_eq!(m.tokens[1].local, "/Users/woong");
    }

    #[test]
    fn reserved_names_rejected() {
        for bad in ["HOME", "ESC", "home"] {
            let mut pm = BTreeMap::new();
            pm.insert("/x".into(), bad.into());
            assert!(PathMapper::new("/Users/woong", &pm).is_err());
        }
    }

    #[test]
    fn token_name_charset_enforced() {
        let mut pm = BTreeMap::new();
        pm.insert("/x".into(), "bad-name".into());
        assert!(PathMapper::new("/Users/woong", &pm).is_err());
    }

    #[test]
    fn windows_home_trailing_sep_trimmed_and_enc() {
        let m = PathMapper::new("C:\\Users\\Loki\\", &BTreeMap::new()).unwrap();
        assert_eq!(m.tokens[0].local, "C:\\Users\\Loki");
        assert_eq!(m.tokens[0].enc_local, "C--Users-Loki");
    }
}
```

- [x] **Step 3: Run to verify fail** — `cargo test mapper` → FAIL (types undefined)

- [x] **Step 4: Implement**

```rust
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MapperError {
    #[error("token name {0:?} is reserved")]
    Reserved(String),
    #[error("invalid token name {0:?}: use [A-Z][A-Z0-9_]*")]
    BadName(String),
}

#[derive(Debug, Clone)]
pub struct TokenMapping {
    pub name: String,
    pub local: String,     // canonical case, no trailing separator
    pub enc_local: String, // Claude Code dir-encoding of `local`
}

#[derive(Debug, Clone)]
pub struct PathMapper {
    pub tokens: Vec<TokenMapping>,
}

pub fn encode_claude_path(p: &str) -> String {
    p.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn valid_name(n: &str) -> bool {
    let mut ch = n.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_uppercase())
        && ch.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl PathMapper {
    pub fn new(home: &str, path_map: &BTreeMap<String, String>) -> Result<Self, MapperError> {
        let mut tokens = Vec::new();
        let trim = |s: &str| s.trim_end_matches(['/', '\\']).to_string();
        for (local, name) in path_map {
            if name.eq_ignore_ascii_case("HOME") || name.eq_ignore_ascii_case("ESC") {
                return Err(MapperError::Reserved(name.clone()));
            }
            if !valid_name(name) {
                return Err(MapperError::BadName(name.clone()));
            }
            let local = trim(local);
            let enc_local = encode_claude_path(&local);
            tokens.push(TokenMapping { name: name.clone(), local, enc_local });
        }
        let home = trim(home);
        let enc_local = encode_claude_path(&home);
        tokens.push(TokenMapping { name: "HOME".into(), local: home, enc_local });
        tokens.sort_by(|a, b| b.local.len().cmp(&a.local.len()));
        Ok(PathMapper { tokens })
    }
}
```

- [x] **Step 5: Run tests pass** — `cargo test mapper` → 4 passed
- [x] **Step 6: Commit** — `git add -A && git commit -m "feat: scaffold + PathMapper with reserved-name validation"`

---

### Task 2: JSON string-span lexer

**Files:**
- Create: `src/transform/mod.rs`, `src/transform/json_spans.rs`; add `pub mod transform;` to `src/lib.rs`
- Test: inline + proptest

**Interfaces:**
- Produces: `StrSpan { pub start: usize, pub end: usize }` (byte range of the escaped content, quotes excluded), `string_spans(line: &[u8]) -> Result<Vec<StrSpan>, LexError>`, `decode_json_string(raw: &[u8]) -> Result<String, LexError>`, `encode_json_string(s: &str) -> Vec<u8>` (minimal escaping: `"` `\` control chars; non-ASCII emitted raw UTF-8).

- [x] **Step 1: Failing tests**

`src/transform/mod.rs`:
```rust
pub mod json_spans;
```

Tests in `json_spans.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_keys_and_values() {
        let line = br#"{"cwd":"/Users/woong","n":3,"ok":true}"#;
        let spans = string_spans(line).unwrap();
        let texts: Vec<String> = spans.iter()
            .map(|s| decode_json_string(&line[s.start..s.end]).unwrap())
            .collect();
        assert_eq!(texts, vec!["cwd", "/Users/woong", "n", "ok"]);
    }

    #[test]
    fn handles_escapes_and_unicode() {
        let line = br#"{"p":"C:\\Users\\Loki\\한글"}"#;
        let spans = string_spans(line).unwrap();
        let v = decode_json_string(&line[spans[1].start..spans[1].end]).unwrap();
        assert_eq!(v, "C:\\Users\\Loki\\한글");
    }

    #[test]
    fn rejects_truncated_line() {
        assert!(string_spans(br#"{"cwd":"/Users/wo"#).is_err());
    }

    #[test]
    fn encode_roundtrip() {
        let s = "C:\\x \"q\" 한글\n";
        let enc = encode_json_string(s);
        assert_eq!(decode_json_string(&enc).unwrap(), s);
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        // Oracle: on any serde-accepted JSON line, our spans decode to exactly
        // the strings serde sees (keys+values, document order).
        #[test]
        fn oracle_matches_serde(v in proptest::string::string_regex("[ -~한-힣\\\\\"]{0,40}").unwrap(),
                                k in "[a-z]{1,8}") {
            let line = serde_json::to_vec(&serde_json::json!({ k.clone(): v.clone(), "z": [v.clone(), 1, null] })).unwrap();
            let spans = string_spans(&line).unwrap();
            let mine: Vec<String> = spans.iter().map(|s| decode_json_string(&line[s.start..s.end]).unwrap()).collect();
            prop_assert_eq!(mine, vec![k, v.clone(), "z".to_string(), v]);
        }
    }
}
```

- [x] **Step 2: Run fail** — `cargo test json_spans` → FAIL
- [x] **Step 3: Implement**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LexError {
    #[error("truncated or malformed JSON at byte {0}")]
    Malformed(usize),
    #[error("invalid escape at byte {0}")]
    BadEscape(usize),
    #[error("invalid utf8/surrogate at byte {0}")]
    BadUnicode(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrSpan { pub start: usize, pub end: usize }

/// Scan a single JSON document (one JSONL line) and return byte ranges of the
/// escaped CONTENT of every string token (keys and values), quotes excluded.
/// Non-string bytes are only validated enough to find string boundaries.
pub fn string_spans(line: &[u8]) -> Result<Vec<StrSpan>, LexError> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'"' => {
                let start = i + 1;
                i += 1;
                loop {
                    if i >= line.len() { return Err(LexError::Malformed(i)); }
                    match line[i] {
                        b'\\' => {
                            if i + 1 >= line.len() { return Err(LexError::Malformed(i)); }
                            i += 2;
                        }
                        b'"' => { spans.push(StrSpan { start, end: i }); i += 1; break; }
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
    if spans.is_empty() && line.iter().any(|&b| b == b'"') {
        return Err(LexError::Malformed(0));
    }
    Ok(spans)
}

pub fn decode_json_string(raw: &[u8]) -> Result<String, LexError> {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            i += 1;
            if i >= raw.len() { return Err(LexError::BadEscape(i)); }
            match raw[i] {
                b'"' => out.push('"'), b'\\' => out.push('\\'), b'/' => out.push('/'),
                b'b' => out.push('\u{8}'), b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'), b'r' => out.push('\r'), b't' => out.push('\t'),
                b'u' => {
                    let hex = |j: usize| -> Result<u32, LexError> {
                        if j + 4 > raw.len() { return Err(LexError::BadUnicode(j)); }
                        u32::from_str_radix(std::str::from_utf8(&raw[j..j+4]).map_err(|_| LexError::BadUnicode(j))?, 16)
                            .map_err(|_| LexError::BadUnicode(j))
                    };
                    let mut cp = hex(i + 1)?;
                    i += 4;
                    if (0xD800..0xDC00).contains(&cp) {
                        if raw.get(i+1) == Some(&b'\\') && raw.get(i+2) == Some(&b'u') {
                            let lo = hex(i + 3)?;
                            if !(0xDC00..0xE000).contains(&lo) { return Err(LexError::BadUnicode(i)); }
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            i += 6;
                        } else { return Err(LexError::BadUnicode(i)); }
                    } else if (0xDC00..0xE000).contains(&cp) {
                        return Err(LexError::BadUnicode(i));
                    }
                    out.push(char::from_u32(cp).ok_or(LexError::BadUnicode(i))?);
                }
                _ => return Err(LexError::BadEscape(i)),
            }
            i += 1;
        } else {
            let s = std::str::from_utf8(&raw[i..]).map_err(|_| LexError::BadUnicode(i))?;
            let c = s.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    Ok(out)
}

pub fn encode_json_string(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.extend(b"\\\""), '\\' => out.extend(b"\\\\"),
            '\n' => out.extend(b"\\n"), '\r' => out.extend(b"\\r"), '\t' => out.extend(b"\\t"),
            c if (c as u32) < 0x20 => out.extend(format!("\\u{:04x}", c as u32).into_bytes()),
            c => { let mut b = [0u8; 4]; out.extend(c.encode_utf8(&mut b).as_bytes()); }
        }
    }
    out
}
```

- [x] **Step 4: Run pass** — `cargo test json_spans` (incl. proptest) → all pass
- [x] **Step 5: Commit** — `git commit -am "feat: JSON string-span lexer with serde proptest oracle"`

---

### Task 3: Path matcher — normalize / dual resolve

**Files:**
- Create: `src/transform/pathmatch.rs` (add to `transform/mod.rs`)

**Interfaces:**
- Produces:
```rust
pub struct SpanRecord { pub original: String }          // exact matched text (home+run)
pub struct NormalizedText { pub text: String, pub spans: Vec<SpanRecord> }
pub enum ResolveMode<'a> { Verify(&'a [SpanRecord]), Pull }
pub fn normalize_text(input: &str, m: &PathMapper) -> NormalizedText
pub fn resolve_text(input: &str, m: &PathMapper, mode: ResolveMode) -> String
```
- Semantics: `normalize_text` (1) escapes literal `${`→`${ESC}`, (2) case-insensitively finds each token's `local` (both `\` and `/` separator variants for drive-letter locals) with boundary check (next char not in path-run charset ⇒ exact-home also matches), (3) captures the following path-run, (4) replaces with `${NAME}` + run with separators normalized to `/`, recording the original span text in order. `Verify` mode re-splices recorded originals per token occurrence (in order); `Pull` mode substitutes canonical local with `/` separators and converts run separators to `/`; both then unescape `${ESC}`→`${` last.

- [x] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::PathMapper;
    use std::collections::BTreeMap;

    fn mac() -> PathMapper { PathMapper::new("/Users/woong", &BTreeMap::new()).unwrap() }
    fn win() -> PathMapper { PathMapper::new("C:\\Users\\Loki", &BTreeMap::new()).unwrap() }

    #[test]
    fn mac_path_tokenized() {
        let n = normalize_text("cd /Users/woong/workspace/foo now", &mac());
        assert_eq!(n.text, "cd ${HOME}/workspace/foo now");
        assert_eq!(n.spans[0].original, "/Users/woong/workspace/foo");
    }

    #[test]
    fn win_backslash_normalized_to_slash() {
        let n = normalize_text(r"C:\Users\Loki\workspace\foo", &win());
        assert_eq!(n.text, "${HOME}/workspace/foo");
    }

    #[test]
    fn case_insensitive_match_canonical_out() {
        let n = normalize_text(r"c:\users\loki\ws", &win());
        assert_eq!(n.text, "${HOME}/ws");
        assert_eq!(n.spans[0].original, r"c:\users\loki\ws"); // original case preserved
    }

    #[test]
    fn boundary_no_match_inside_longer_name() {
        let n = normalize_text("/Users/woongho/app", &mac());
        assert_eq!(n.text, "/Users/woongho/app");
        assert!(n.spans.is_empty());
    }

    #[test]
    fn other_os_home_untouched() {
        let n = normalize_text(r"log said C:\Users\Loki\x", &mac());
        assert_eq!(n.text, r"log said C:\Users\Loki\x");
    }

    #[test]
    fn literal_token_escaped_injective() {
        let n = normalize_text("run ${HOME}/bin and ${ESC} too", &mac());
        assert_eq!(n.text, "run ${ESC}HOME}/bin and ${ESC}ESC} too");
        // pull restores literals
        assert_eq!(resolve_text(&n.text, &mac(), ResolveMode::Pull), "run ${HOME}/bin and ${ESC} too");
    }

    #[test]
    fn verify_roundtrip_byte_exact_mixed() {
        for (m, s) in [(mac(), "a /Users/woong/x b ${HOME} c /Users/woong"),
                       (win(), r#"cwd C:\Users\Loki\p and c:/users/loki/q"#)] {
            let n = normalize_text(s, &m);
            assert_eq!(resolve_text(&n.text, &m, ResolveMode::Verify(&n.spans)), s);
        }
    }

    #[test]
    fn pull_on_windows_emits_forward_slash() {
        let r = resolve_text("${HOME}/workspace/foo", &win(), ResolveMode::Pull);
        assert_eq!(r, "C:/Users/Loki/workspace/foo");
    }

    #[test]
    fn unknown_token_passes_through_in_content() {
        let r = resolve_text("echo ${PATH} and ${WORK}/x", &mac(), ResolveMode::Pull);
        assert_eq!(r, "echo ${PATH} and ${WORK}/x");
    }

    #[test]
    fn korean_run_kept() {
        let n = normalize_text("/Users/woong/문서/메모.md", &mac());
        assert_eq!(n.text, "${HOME}/문서/메모.md");
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use crate::mapper::PathMapper;
    use std::collections::BTreeMap;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn invariants_a_and_b(seg in "[a-zA-Z0-9_./\\\\-]{0,24}", pre in "[ -~]{0,12}") {
            for home in ["/Users/woong", "C:\\Users\\Loki"] {
                let m = PathMapper::new(home, &BTreeMap::new()).unwrap();
                let input = format!("{pre} {home}{sep}{seg}", sep = if home.starts_with('/') {"/"} else {"\\"});
                let n = normalize_text(&input, &m);
                // A: byte round-trip
                prop_assert_eq!(resolve_text(&n.text, &m, ResolveMode::Verify(&n.spans)), input.clone());
                // B: fixpoint
                let pulled = resolve_text(&n.text, &m, ResolveMode::Pull);
                prop_assert_eq!(normalize_text(&pulled, &m).text, n.text);
            }
        }
    }
}
```

- [x] **Step 2: Run fail** — `cargo test pathmatch` → FAIL
- [x] **Step 3: Implement**

```rust
use crate::mapper::PathMapper;

#[derive(Debug, Clone)]
pub struct SpanRecord { pub original: String }

#[derive(Debug)]
pub struct NormalizedText { pub text: String, pub spans: Vec<SpanRecord> }

pub enum ResolveMode<'a> { Verify(&'a [SpanRecord]), Pull }

const ESC: &str = "${ESC}";

fn is_run_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | '~' | '+' | '@') || !c.is_ascii()
}

/// Case-insensitive prefix match of `local` (with both separator variants)
/// at `pos`; returns matched length in bytes if boundary holds.
fn match_local_at(text: &str, pos: usize, local: &str) -> Option<usize> {
    let rest = &text[pos..];
    let mut ri = rest.chars();
    let mut matched = 0usize;
    for lc in local.chars() {
        let rc = ri.next()?;
        let sep_ok = matches!(lc, '/' | '\\') && matches!(rc, '/' | '\\');
        if !sep_ok && rc.to_lowercase().to_string() != lc.to_lowercase().to_string() {
            return None;
        }
        matched += rc.len_utf8();
    }
    // boundary: next char must be a separator, non-run char, or end
    match rest[matched..].chars().next() {
        None => Some(matched),
        Some(c) if matches!(c, '/' | '\\') => Some(matched),
        Some(c) if !is_run_char(c) => Some(matched),
        _ => None,
    }
}

pub fn normalize_text(input: &str, m: &PathMapper) -> NormalizedText {
    // Phase 1: injectivity escape
    let escaped = input.replace("${", ESC);
    // Phase 2: scan & replace
    let mut out = String::with_capacity(escaped.len());
    let mut spans = Vec::new();
    let mut i = 0;
    'outer: while i < escaped.len() {
        for t in &m.tokens {
            if let Some(len) = match_local_at(&escaped, i, &t.local) {
                let run_start = i + len;
                let mut run_end = run_start;
                for c in escaped[run_start..].chars() {
                    if is_run_char(c) { run_end += c.len_utf8(); } else { break; }
                }
                // Original span must be recovered from pre-escape input: since
                // ESC substitution only rewrites "${", and "${" cannot occur
                // inside a matched local+run (run charset excludes '{'), the
                // escaped slice equals the original slice here.
                let original = escaped[i..run_end].to_string();
                out.push_str(&format!("${{{}}}", t.name));
                out.push_str(&escaped[run_start..run_end].replace('\\', "/"));
                spans.push(SpanRecord { original });
                i = run_end;
                continue 'outer;
            }
        }
        let c = escaped[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    NormalizedText { text: out, spans }
}

pub fn resolve_text(input: &str, m: &PathMapper, mode: ResolveMode) -> String {
    let mut out = String::with_capacity(input.len());
    let mut span_idx = 0usize;
    let mut i = 0;
    'outer: while i < input.len() {
        if input[i..].starts_with(ESC) {
            // handled at the end — copy through for now
        } else if input[i..].starts_with("${") {
            for t in &m.tokens {
                let tok = format!("${{{}}}", t.name);
                if input[i..].starts_with(&tok) {
                    let run_start = i + tok.len();
                    let mut run_end = run_start;
                    for c in input[run_start..].chars() {
                        if is_run_char(c) { run_end += c.len_utf8(); } else { break; }
                    }
                    match &mode {
                        ResolveMode::Verify(spans) => {
                            out.push_str(&spans[span_idx].original);
                            span_idx += 1;
                        }
                        ResolveMode::Pull => {
                            out.push_str(&t.local.replace('\\', "/"));
                            out.push_str(&input[run_start..run_end]); // already '/'
                        }
                    }
                    i = run_end;
                    continue 'outer;
                }
            }
        }
        let c = input[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out.replace(ESC, "${")
}
```

- [x] **Step 4: Run pass** — `cargo test pathmatch` → all pass (fix the implementation, never weaken a test; if a proptest case fails, minimize and add as a named regression test)
- [x] **Step 5: Commit** — `git commit -am "feat: path matcher with injective escaping and dual resolve"`

---

### Task 4: Directory-key transform

**Files:**
- Create: `src/transform/dirkey.rs`

**Interfaces:**
- Produces: `key_to_portable(seg: &str, m: &PathMapper) -> String`, `portable_to_key(seg: &str, m: &PathMapper) -> Result<String, UnmappedToken>`, `pub struct UnmappedToken(pub String)`. Uses `mapper::encode_claude_path`. Boundary rule: `seg == enc_local || seg.starts_with(&(enc_local + "-"))`, case-insensitive compare, matched prefix replaced by `${NAME}`.

- [x] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::PathMapper;
    use std::collections::BTreeMap;

    fn mac() -> PathMapper { PathMapper::new("/Users/woong", &BTreeMap::new()).unwrap() }
    fn win() -> PathMapper { PathMapper::new("C:\\Users\\Loki", &BTreeMap::new()).unwrap() }

    #[test]
    fn roundtrip_both_oses() {
        let p = key_to_portable("-Users-woong-workspace-foo", &mac());
        assert_eq!(p, "${HOME}-workspace-foo");
        assert_eq!(portable_to_key(&p, &win()).unwrap(), "C--Users-Loki-workspace-foo");
        assert_eq!(portable_to_key(&p, &mac()).unwrap(), "-Users-woong-workspace-foo");
    }

    #[test]
    fn boundary_blocks_woongho() {
        assert_eq!(key_to_portable("-Users-woongho-app", &mac()), "-Users-woongho-app");
    }

    #[test]
    fn exact_home_key() {
        assert_eq!(key_to_portable("-Users-woong", &mac()), "${HOME}");
    }

    #[test]
    fn case_insensitive_key_match() {
        assert_eq!(key_to_portable("C--users-loki-ws", &win()), "${HOME}-ws");
    }

    #[test]
    fn unknown_token_is_error() {
        assert!(matches!(portable_to_key("${WORK}-x", &mac()), Err(UnmappedToken(_))));
    }
}
```

- [x] **Step 2: Run fail** → FAIL
- [x] **Step 3: Implement**

```rust
use crate::mapper::PathMapper;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("no local mapping for token ${{{0}}} on this device")]
pub struct UnmappedToken(pub String);

pub fn key_to_portable(seg: &str, m: &PathMapper) -> String {
    for t in &m.tokens {
        let enc = &t.enc_local;
        let seg_l = seg.to_lowercase();
        let enc_l = enc.to_lowercase();
        if seg_l == enc_l {
            return format!("${{{}}}", t.name);
        }
        if seg_l.starts_with(&format!("{enc_l}-")) {
            return format!("${{{}}}{}", t.name, &seg[enc.len()..]);
        }
    }
    seg.to_string()
}

pub fn portable_to_key(seg: &str, m: &PathMapper) -> Result<String, UnmappedToken> {
    if !seg.starts_with("${") { return Ok(seg.to_string()); }
    for t in &m.tokens {
        let tok = format!("${{{}}}", t.name);
        if let Some(rest) = seg.strip_prefix(&tok) {
            return Ok(format!("{}{}", t.enc_local, rest));
        }
    }
    let name = seg[2..].split('}').next().unwrap_or("?").to_string();
    Err(UnmappedToken(name))
}
```

- [x] **Step 4: Run pass**, **Step 5: Commit** — `git commit -am "feat: dirkey portable mapping with boundary rule"`

---

### Task 5: File-level transform assembly + verify gates

**Files:**
- Create: `src/transform/file.rs`, `src/verify.rs` (add `pub mod verify;` to lib)

**Interfaces:**
- Produces:
```rust
pub enum FileKind { Jsonl, Json, PlainText, FileHistorySnapshot, Unknown }
pub fn classify(rel_path: &str) -> FileKind   // by extension + "file-history/" prefix rule
pub enum TransformOutcome {
    Transformed { data: Vec<u8>, spans: Vec<Vec<SpanRecord>> }, // per line
    Verbatim { reason: String },
}
pub fn normalize_file(rel_path: &str, data: &[u8], m: &PathMapper) -> TransformOutcome
pub fn resolve_file_pull(rel_path: &str, data: &[u8], m: &PathMapper) -> Result<Vec<u8>, UnmappedToken>
// verify.rs
pub fn push_gate(rel_path: &str, original: &[u8], m: &PathMapper) -> TransformOutcome
// runs normalize_file; on Transformed, checks invariant A per line (verify-resolve
// re-splice == original line) and invariant B (normalize(pull_resolve(line)) == line-normalized).
// Any line failing a parse ⇒ that line passes through byte-identical (line-level fail-closed).
// Any invariant failure ⇒ whole-file Verbatim{reason}.
```
- JSONL processing: split on `\n` (preserve exact line bytes incl. trailing `\r`); per line: `string_spans` → decode each → `normalize_text` on decoded → re-encode ONLY changed strings → splice. Lex error on a line ⇒ that line verbatim. `.md`/`.txt`: `normalize_text` on whole (lossy-utf8 rejected ⇒ Verbatim). `FileHistorySnapshot` (any path under `file-history/` not ending in `.json`): always Verbatim (undo bytes sacred). `Unknown`: Verbatim.

- [x] **Step 1: Failing tests** — key cases (write all in `src/transform/file.rs` tests):

```rust
#[test]
fn jsonl_cwd_and_key_paths_rewritten() {
    let m = mac();
    let line = br#"{"cwd":"/Users/woong/ws/app","trackedFileBackups":{"/Users/woong/ws/app/src/main.rs":"h1"}}"#;
    match normalize_file("projects/x/s.jsonl", line, &m) {
        TransformOutcome::Transformed { data, .. } => {
            let s = String::from_utf8(data).unwrap();
            assert!(s.contains(r#""cwd":"${HOME}/ws/app""#));
            assert!(s.contains(r#""${HOME}/ws/app/src/main.rs""#));
        }
        _ => panic!("expected transform"),
    }
}

#[test]
fn truncated_last_line_passes_verbatim_line_level() {
    let m = mac();
    let data = b"{\"cwd\":\"/Users/woong/a\"}\n{\"cwd\":\"/Users/wo";
    match normalize_file("projects/x/s.jsonl", data, &m) {
        TransformOutcome::Transformed { data: out, .. } => {
            let s = String::from_utf8(out).unwrap();
            assert!(s.starts_with("{\"cwd\":\"${HOME}/a\"}\n"));
            assert!(s.ends_with("{\"cwd\":\"/Users/wo")); // untouched
        }
        _ => panic!(),
    }
}

#[test]
fn file_history_snapshot_always_verbatim() {
    let m = mac();
    let r = normalize_file("file-history/abc/1.md", b"path /Users/woong/x", &m);
    assert!(matches!(r, TransformOutcome::Verbatim { .. }));
}

#[test]
fn push_gate_roundtrips_windows_content() {
    let m = win();
    let line = br#"{"cwd":"C:\\Users\\Loki\\ws\\app"}"#;
    // must NOT degrade to verbatim (the C1 regression test)
    assert!(matches!(push_gate("projects/x/s.jsonl", line, &m),
                     TransformOutcome::Transformed { .. }));
}

#[test]
fn pull_resolve_emits_slash_form_in_json() {
    let m = win();
    let portable = br#"{"cwd":"${HOME}/ws/app"}"#;
    let out = resolve_file_pull("projects/x/s.jsonl", portable, &m).unwrap();
    assert_eq!(out, br#"{"cwd":"C:/Users/Loki/ws/app"}"#.to_vec());
}
```

- [x] **Step 2: Run fail** → FAIL
- [x] **Step 3: Implement** — structure (complete the obvious plumbing exactly as described in Interfaces; the splice loop):

```rust
fn transform_line(line: &[u8], m: &PathMapper) -> Result<(Vec<u8>, Vec<SpanRecord>), LexError> {
    let spans = string_spans(line)?;
    let mut out = Vec::with_capacity(line.len());
    let mut records = Vec::new();
    let mut cursor = 0usize;
    for sp in spans {
        out.extend_from_slice(&line[cursor..sp.start]);
        let decoded = decode_json_string(&line[sp.start..sp.end])?;
        let n = normalize_text(&decoded, m);
        if n.spans.is_empty() {
            out.extend_from_slice(&line[sp.start..sp.end]); // untouched bytes
        } else {
            out.extend(encode_json_string(&n.text));
            records.extend(n.spans);
        }
        cursor = sp.end;
    }
    out.extend_from_slice(&line[cursor..]);
    Ok((out, records))
}
```
`normalize_file` iterates lines with `split_inclusive(|&b| b == b'\n')`; lex error ⇒ push original line bytes + empty records. `resolve_file_pull` mirrors it with `resolve_text(.., Pull)` per changed string (a string is "changed" iff it contains `${`). `push_gate` implements invariants A and B per Interfaces; on any mismatch returns `Verbatim { reason: format!("round-trip mismatch at line {}", idx) }`.

- [x] **Step 4: Run pass** — `cargo test transform verify` → pass
- [x] **Step 5: Commit** — `git commit -am "feat: file-level transform with line fail-closed and C1 dual-resolve gate"`

---

### Task 6: Crypto

**Files:**
- Create: `src/crypto.rs`

**Interfaces:**
- Produces: `derive(passphrase: &str, salt: &[u8; 32]) -> Keys` where `Keys { identity: age::x25519::Identity, recipient: age::x25519::Recipient, hmac_key: [u8; 32] }` (Argon2id m=65536,t=3,p=1 → 64 bytes: first 32 clamped → x25519 scalar, last 32 = hmac_key); `seal(plain: &[u8], r: &Recipient) -> Vec<u8>` (gzip level 6 then age); `open(sealed: &[u8], id: &Identity) -> anyhow::Result<Vec<u8>>`; `object_name(hmac_key: &[u8;32], portable_path: &str) -> String` (hex HMAC-SHA256).

- [x] **Step 1: Failing tests**

```rust
#[test]
fn same_passphrase_same_keys_across_devices() {
    let salt = [7u8; 32];
    let a = derive("hunter2", &salt);
    let b = derive("hunter2", &salt);
    assert_eq!(a.recipient.to_string(), b.recipient.to_string());
    assert_eq!(a.hmac_key, b.hmac_key);
    assert_ne!(derive("hunter2", &[8u8; 32]).recipient.to_string(), a.recipient.to_string());
}

#[test]
fn seal_open_roundtrip_and_nondeterminism() {
    let k = derive("pw", &[1u8; 32]);
    let msg = b"hello \xEA\xB0\x80"; // includes UTF-8 Korean bytes
    let s1 = seal(msg, &k.recipient);
    let s2 = seal(msg, &k.recipient);
    assert_ne!(s1, s2); // age is non-deterministic — this is WHY state.json tracks plaintext hashes
    assert_eq!(open(&s1, &k.identity).unwrap(), msg.to_vec());
}

#[test]
fn wrong_passphrase_fails_closed() {
    let k1 = derive("pw", &[1u8; 32]);
    let k2 = derive("pw2", &[1u8; 32]);
    assert!(open(&seal(b"x", &k1.recipient), &k2.identity).is_err());
}

#[test]
fn object_names_stable_and_keyed() {
    let k = derive("pw", &[1u8; 32]);
    let n1 = object_name(&k.hmac_key, "settings.json");
    assert_eq!(n1, object_name(&k.hmac_key, "settings.json"));
    assert_ne!(n1, object_name(&derive("other", &[1u8;32]).hmac_key, "settings.json"));
    assert!(n1.chars().all(|c| c.is_ascii_hexdigit()));
}
```

- [x] **Step 2: Run fail**, **Step 3: Implement** (age scryptless x25519 path: clamp bytes per RFC 7748: `b[0] &= 248; b[31] &= 127; b[31] |= 64;` then `age::x25519::Identity` from `StaticSecret`; gzip via `flate2::write::GzEncoder`), **Step 4: Run pass**
- [x] **Step 5: Commit** — `git commit -am "feat: passphrase-derived age crypto with HMAC object naming"`

---

### Task 7: Manifest + chunking

**Files:**
- Create: `src/manifest.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Serialize, Deserialize)]
pub struct Manifest { pub version: u32, pub last_push_device: String, pub last_push_ts: u64,
                      pub entries: BTreeMap<String, Entry> }   // key = portable path
#[derive(Serialize, Deserialize)]
pub struct Entry { pub object: String, pub chunks: u32, pub plaintext_hash: String,
                   pub size: u64, pub mode: EntryMode }
#[derive(Serialize, Deserialize, PartialEq)]
pub enum EntryMode { Transformed, Verbatim }
pub const CHUNK_SIZE: usize = 90 * 1024 * 1024;
pub fn chunk_paths(object: &str, chunks: u32) -> Vec<String> // ["<o>.age"] or ["<o>.age.0", ...]
pub fn split_chunks(sealed: &[u8]) -> Vec<&[u8]>
```
- Manifest serialized as JSON, then sealed with Task 6 crypto to `manifest.age`.

- [x] **Step 1: Failing tests** — serde roundtrip; `split_chunks` of 200MiB dummy → 3 chunks, concat == original; `chunk_paths("ab", 1) == ["objects/ab.age"]`, `chunk_paths("ab", 3) == ["objects/ab.age.0","objects/ab.age.1","objects/ab.age.2"]`.
- [x] **Step 2: Run fail**, **Step 3: Implement** (direct), **Step 4: Run pass**
- [x] **Step 5: Commit** — `git commit -am "feat: encrypted manifest model with 90MiB chunking"`

---

### Task 8: Config + State + Scanner

**Files:**
- Create: `src/config.rs`, `src/state.rs`, `src/scan.rs`

**Interfaces:**
- Produces:
```rust
// config.rs — ~/.claude-xsync/config.toml
#[derive(Serialize, Deserialize)]
pub struct Config { pub remote: String, pub device: String,
                    pub path_map: BTreeMap<String, String>,   // local path → TOKEN
                    pub extra_paths: Vec<String>, pub removed_paths: Vec<String>,
                    pub history_mode: HistoryMode }           // Keep | Snapshot
pub fn claude_dir() -> PathBuf  // env XSYNC_CLAUDE_DIR override
pub fn xsync_dir() -> PathBuf   // env XSYNC_DIR override
pub fn home_dir() -> PathBuf    // env XSYNC_HOME override, else dirs::home_dir()
// state.rs — ~/.claude-xsync/state.json
#[derive(Serialize, Deserialize, Default)]
pub struct State { pub files: BTreeMap<String, String>,  // portable path → plaintext sha256 hex
                   pub last_synced_commit: Option<String> }
pub fn load_state() -> State; pub fn save_state(&State) -> anyhow::Result<()>;
pub fn upsert_and_save(state: &mut State, portable: &str, hash: &str) -> anyhow::Result<()> // per-file immediate write
// scan.rs
pub const ALLOWLIST: &[&str] = &["projects", "history.jsonl", "file-history", "tasks", "todos",
    "plans", "settings.json", "settings.local.json", "CLAUDE.md", "keybindings.json",
    "agents", "skills", "commands", "rules", "workflows"];
pub const EXCLUDED: &[&str] = &["ide", "session-env", "sessions", "shell-snapshots", "chrome",
    "cache", "paste-cache", "debug", "downloads", "backups", "channels",
    "stats-cache.json", "mcp-needs-auth-cache.json", ".credentials.json", "plugins", ".claude.json"];
pub struct ScanResult { pub files: Vec<(String /*rel*/, PathBuf)>, pub unknown: Vec<String> }
pub fn scan(claude_dir: &Path, cfg: &Config) -> anyhow::Result<ScanResult>
// rel paths use '/' separators on all OSes (normalize with components()).
pub fn sha256_file(p: &Path) -> anyhow::Result<String>
```
- `plugins` and `.claude.json` are in EXCLUDED here because Task 10 handles them as synthetic files; scanner itself never walks them. Unknown = top-level entries in neither list nor `removed_paths`.

- [x] **Step 1: Failing tests** — use `tempfile::tempdir()` as fake claude_dir: create `projects/a/s.jsonl`, `settings.json`, `ide/x`, `weird-new-dir/f`; assert scan returns exactly the two allowlisted rel paths with `/` separators and `unknown == ["weird-new-dir"]`. State upsert writes file immediately (read back from disk in test).
- [x] **Step 2: Run fail**, **Step 3: Implement** (walk with `std::fs`, recurse allowlisted dirs; skip symlinks), **Step 4: Run pass**
- [x] **Step 5: Commit** — `git commit -am "feat: config/state/scanner with allowlist and unknown detection"`

---

### Task 9: fsx + gitx + procguard

**Files:**
- Create: `src/fsx.rs`, `src/gitx.rs`, `src/procguard.rs`

**Interfaces:**
- Produces:
```rust
// fsx.rs
pub fn atomic_write(path: &Path, data: &[u8]) -> anyhow::Result<()>   // temp in same dir + rename; create parents
pub fn backup_files(claude_dir: &Path, rels: &[String], stamp: &str) -> anyhow::Result<PathBuf>
// → copies each existing rel into ~/.claude.backup.<stamp>/<rel>
pub fn long_path(p: &Path) -> PathBuf  // Windows: prefix \\?\ when len > 240; no-op elsewhere
// gitx.rs — all via std::process::Command("git"), cwd = repo dir
pub struct Git { pub repo: PathBuf }
impl Git {
    pub fn clone_or_open(remote: &str, repo: &Path) -> anyhow::Result<Git>;
    pub fn fetch(&self) -> anyhow::Result<()>;
    pub fn head(&self) -> anyhow::Result<String>;
    pub fn remote_head(&self) -> anyhow::Result<Option<String>>;          // origin/main
    pub fn is_ancestor(&self, a: &str, b: &str) -> anyhow::Result<bool>;  // rev-list check
    pub fn diverged(&self) -> anyhow::Result<bool>;   // neither is ancestor of the other (force-push signal)
    pub fn reset_hard_origin(&self) -> anyhow::Result<()>;
    pub fn commit_all(&self, msg: &str) -> anyhow::Result<String>;
    pub fn push(&self) -> anyhow::Result<()>; pub fn push_force(&self) -> anyhow::Result<()>;
    pub fn pull_ff(&self) -> anyhow::Result<()>;      // fetch + merge --ff-only
}
// procguard.rs
pub fn claude_running(claude_dir: &Path) -> Vec<u32>  // sessions/*.json → pid alive via sysinfo
```
- gitx errors must include git's stderr verbatim in the anyhow context.

- [x] **Step 1: Failing tests** — gitx integration test in `tests/gitx.rs`: create `tempdir` bare repo (`git init --bare`), `clone_or_open`, write file, `commit_all`, `push`, second clone sees it; simulate force-push then assert `diverged()==true` and `reset_hard_origin()` recovers. fsx: atomic_write to nested missing dirs; backup copies only existing files. procguard: fabricated sessions dir with own `std::process::id()` → detected; pid 999999 → not.
- [x] **Step 2: Run fail**, **Step 3: Implement**, **Step 4: Run pass** — `cargo test --test gitx fsx procguard`
- [x] **Step 5: Commit** — `git commit -am "feat: git shell-out, atomic fs, running-instance guard"`

---

### Task 10: Special files — mcp / plugins / history merge

**Files:**
- Create: `src/special/mod.rs`, `src/special/mcp.rs`, `src/special/plugins.rs`, `src/special/history_merge.rs`

**Interfaces:**
- Produces:
```rust
// mcp.rs — pure on strings
pub fn extract_mcp(claude_json: &str) -> anyhow::Result<Option<String>>
// → serde parse, take top-level "mcpServers" subtree, serialize compact. None if absent.
pub fn merge_mcp(claude_json: &str, mcp_subtree: &str) -> anyhow::Result<String>
// → replace/insert ONLY "mcpServers" key, all other keys byte... (serde re-serialize acceptable
//   here: this file is machine-local, never round-trip-verified; still preserve_order via
//   serde_json feature "preserve_order" — ADD to Cargo.toml features)
// Synthetic portable path: "_xsync/mcp-servers.json" (transform pipeline applies as Json kind).
// plugins.rs
pub fn plugin_manifest_rels(plugins_dir: &Path) -> Vec<String>
// → files directly under plugins/ root (e.g. config.json, *.json) — NOT directories.
//   Directories (cache/, repos/, marketplaces/) are machine-built artifacts: excluded.
//   [Implementation-time investigation note from spec §4: verify Claude Code reinstalls
//   from these config files on real machine; adjust the include set there if needed.]
// history_merge.rs — pure
pub fn union_jsonl(local: &[u8], remote: &[u8]) -> Vec<u8>
// → line-set union preserving remote order first then local-only lines appended in local order;
//   exact byte lines as units; trailing partial line of each input preserved at end.
```

- [x] **Step 1: Failing tests** — mcp: extract from fixture JSON with 70 sibling keys returns only subtree; merge into a different claude_json keeps siblings (`userID` etc.) intact and replaces `mcpServers`; roundtrip idempotent. history: overlapping JSONL byte-lines union with no duplicates, remote-first order. plugins: tempdir with `config.json` file + `cache/` dir → only `plugins/config.json` returned.
- [x] **Step 2: Run fail**, **Step 3: Implement**, **Step 4: Run pass**
- [x] **Step 5: Commit** — `git commit -am "feat: mcp subtree extract/merge, plugin manifest filter, history union"`

---

### Task 11: `init` + `push` pipeline

**Files:**
- Create: `src/cli/mod.rs`, `src/cli/init.rs`, `src/cli/push.rs`; rewrite `src/main.rs` (clap derive: subcommands `Init`, `Push{--dry-run,--force}`, `Pull{--dry-run,--force}`, `Status`, `Gc{--squash}`, `Rekey`)
- Test: `tests/e2e.rs` (integration harness)

**Interfaces:**
- Consumes: everything above.
- Produces: `run_push(opts) -> anyhow::Result<i32>` (exit code), e2e harness `TestEnv` reused by Task 12:
```rust
// tests/e2e.rs harness — builds two fake devices sharing one bare repo
struct TestEnv { bare: TempDir, dev_a: FakeDevice, dev_b: FakeDevice }
struct FakeDevice { home: TempDir }   // sets XSYNC_HOME/XSYNC_CLAUDE_DIR/XSYNC_DIR, runs binary via assert-style fn
fn run(dev: &FakeDevice, args: &[&str]) -> (i32, String) // spawns target/debug/claude-xsync with env
```
- `init` flow: prompt-free flags for tests (`--remote <url> --device <name> --passphrase-env XSYNC_PASSPHRASE`): clone-or-init repo, create/read `salt`, derive keys, write config, if remote has manifest → verify decrypt (wrong passphrase = exit 2).
- `push` flow implements spec §7 push 1–5 exactly: procguard → guard A (`remote_head` newer && `manifest.last_push_device != cfg.device` ⇒ exit 2 with "pull first" unless --force) → scan (+unknown warnings) → per file: hash-skip → `push_gate` → seal → chunk write under `objects/` → synthetic files (mcp extract, plugins manifests) through same pipeline → manifest update+seal → commit+push → state upsert per file. Deletions: state keys missing from scan ⇒ drop from manifest. Summary line + exit code per spec §8.

- [x] **Step 1: Failing e2e test**

```rust
#[test]
fn init_and_first_push_populates_remote() {
    let env = TestEnv::new(); // dev_a home has: projects/-{encA}-ws-app/s.jsonl with a cwd line, settings.json
    let (code, _) = run(&env.dev_a, &["init", "--remote", env.bare_url(), "--device", "mac", "--passphrase-env", "XSYNC_PASSPHRASE"]);
    assert_eq!(code, 0);
    let (code, out) = run(&env.dev_a, &["push"]);
    assert_eq!(code, 0);
    assert!(out.contains("synced"));
    // second push = no-op (hash skip)
    let (_, out2) = run(&env.dev_a, &["push"]);
    assert!(out2.contains("✓ 0 synced"));
}

#[test]
fn guard_a_blocks_out_of_order_push() { /* dev_b pushes first; dev_a push → exit 2, message contains "pull" */ }
```

- [x] **Step 2: Run fail** — `cargo test --test e2e` → FAIL
- [x] **Step 3: Implement** init.rs + push.rs per Interfaces (wire modules; no new logic beyond orchestration)
- [x] **Step 4: Run pass** — `cargo test --test e2e`
- [x] **Step 5: Commit** — `git commit -am "feat: init and push pipeline with guard A"`

---

### Task 12: `pull` pipeline + `status`

**Files:**
- Create: `src/cli/pull.rs`, `src/cli/status.rs`
- Test: extend `tests/e2e.rs`

**Interfaces:**
- Consumes: Task 11 harness.
- Produces: `run_pull`, `run_status`. Pull implements spec §7 classification table verbatim:
  - fetch; if `diverged()` → `reset_hard_origin()` + re-anchor: `state.files` re-keyed from freshly decrypted manifest hashes (files whose local hash equals manifest hash get state=hash; others left to classification).
  - decrypt manifest; per entry classify: local-only-new / remote-only-changed / local-only-changed / both-modified / remote-deleted (exact rules from spec §7 table).
  - Stage ALL resolutions in memory/tempdir first (Transformed → `resolve_file_pull` + reverse-verify `normalize == portable`; Verbatim → raw). Any staging failure → skip that file, collect warning, continue.
  - Apply: `backup_files` for to-be-replaced + to-be-deleted rels → atomic_write each (dirkey resolution via `portable_to_key` for `projects/` segments) → both-modified: write remote, save local as `<path>.xsync-conflict.<unix_ts>`; `history.jsonl` → `union_jsonl` result instead → mcp synthetic → `merge_mcp` into real `.claude.json` → remote-deleted → move into backup dir → `upsert_and_save` state per file.
- `status`: fetch (skip on `--offline`), decrypt manifest, print to-push / to-pull / conflicts counts, last push device+time.

- [x] **Step 1: Failing e2e tests**

```rust
#[test]
fn full_cross_device_roundtrip_with_path_rewrite() {
    // dev_a (unix home A) pushes session with cwd $A/ws/app
    // dev_b (unix home B, different name) init+pull
    // assert: projects dir renamed to -<encB>-ws-app, file content contains "<homeB>/ws/app"
}

#[test]
fn forgot_push_scenario_c3_no_deadlock_no_loss() {
    // A: push. B: pull, edit settings.json + append history line, push.
    // A (stale, has local new file + modified history): pull
    // assert exit 0; A's local-only new file preserved; history.jsonl = union;
    // settings.json = remote version + settings.json.xsync-conflict.<ts> exists with A's version
}

#[test]
fn verbatim_mode_files_skip_resolve() { /* push a file that push_gate degrades (unknown-format), pull on B: byte-identical */ }

#[test]
fn squash_recovery() { /* A: gc --squash (Task 13 stub: direct git force-push here) → B: pull → exit 0, files intact */ }
```

- [x] **Step 2: Run fail**, **Step 3: Implement**, **Step 4: Run pass**
- [x] **Step 5: Commit** — `git commit -am "feat: classified pull with conflict preservation and status"`

---

### Task 13: `gc --squash` + `rekey`

**Files:**
- Create: `src/cli/gc.rs`, `src/cli/rekey.rs`

**Interfaces:**
- `gc --squash`: `git checkout --orphan xsync-squash && git add -A && git commit && git branch -M main && push --force`. Prints reminder: "other device will auto-recover on next pull".
- `rekey`: prompt new passphrase (env override for tests) → new salt written → re-derive → re-seal every object + manifest from local plaintext (re-run push pipeline with `--force-all`) → mandatory squash (old-key history removed) → force push.

- [x] **Step 1: Failing e2e tests** — squash: after 3 pushes, `gc --squash` → bare repo `git rev-list --count HEAD == 1`; dev_b pull recovers (replaces Task 12's stub). rekey: after rekey with new passphrase, old passphrase `status` fails exit 2, new passphrase pull on fresh device works, history count == 1.
- [x] **Step 2: Run fail**, **Step 3: Implement**, **Step 4: Run pass**
- [x] **Step 5: Commit** — `git commit -am "feat: gc --squash and rekey with mandatory history purge"`

---

### Task 14: Golden fixtures + masking script

**Files:**
- Create: `tests/fixtures/mac-session.jsonl`, `tests/fixtures/win-session.jsonl`, `tests/golden.rs`, `scripts/mask-fixture.py`

**Interfaces:**
- `scripts/mask-fixture.py <in.jsonl> <out.jsonl>`: structure-preserving masking — parses each line with Python `json`, walks all strings, replaces alphanumeric runs ≥4 chars with same-length `x`/`한` runs EXCEPT (a) substrings matching the session's home-path prefixes (kept verbatim — they are the test subject), (b) JSON structural chars. Prints diff stats. (Complete script ~60 lines; implementer writes it, requirement: output must still parse as JSONL and preserve every path-shaped substring.)
- Fixture provenance: mac fixture = mask a real short session from `~/.claude/projects/` NOW; win fixture = **collect from Loki machine at implementation start** (spec §9.2 — do not defer to release time). Until the real Windows capture lands, generate a synthetic win-session.jsonl by hand covering: `C:\\Users\\Loki\\...` cwd, `trackedFileBackups` path keys, `C:/Users/Loki` mixed form, Korean path, `${HOME}` literal in a bash string, truncated last line. Mark with leading comment line `{"_xsync_fixture":"synthetic-until-real-capture"}` and replace when real capture arrives.
- `tests/golden.rs`: snapshot both directions — normalize(mac fixture, mac mapper) → assert full output equals committed `tests/fixtures/mac-session.portable.jsonl`; then resolve with win mapper → equals committed `mac-session.on-win.jsonl`. Same for win fixture. Regenerate committed outputs via `UPDATE_GOLDEN=1 cargo test golden` guard.

- [x] **Step 1: Write masking script + produce fixtures** (run script on a real session; hand-write synthetic win fixture)
- [x] **Step 2: Write golden test, run with `UPDATE_GOLDEN=1`** to mint snapshots, inspect them manually for correctness (cwd tokenized, literals escaped, Korean preserved)
- [x] **Step 3: Run without env** — `cargo test --test golden` → pass
- [x] **Step 4: Commit** — `git commit -am "test: golden cross-OS fixtures with masking script"`

---

### Task 15: CI + release + README/LICENSE

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `README.md`, `LICENSE`

**Interfaces:**
- `ci.yml`: on push/PR — matrix `{macos-latest, windows-latest, ubuntu-latest}` × `cargo test --all-targets` + `cargo fmt --check` + `cargo clippy -- -D warnings`.
- `release.yml`: on tag `v*` — build 6 targets (`aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`), upload ALL to the GitHub Release, then **verification job**: `gh release view --json assets` → fail the workflow if asset count < 7 (6 binaries + checksums.txt). This step is non-negotiable (spec §9 — the tawanorg lesson).
- `README.md`: what/why (cross-OS path correctness — cite the two prior tools' gaps using spec §1's corrected wording), quickstart (init/push/pull), limitations section copied from spec §10, credits to tawanorg/claude-sync and claude-context-sync. `LICENSE`: MIT, copyright 2026 (repo owner name).

- [ ] **Step 1: Write both workflows + README + LICENSE**
- [ ] **Step 2: Verify locally** — `cargo fmt --check && cargo clippy -- -D warnings && cargo test` all green
- [ ] **Step 3: Commit** — `git commit -am "chore: CI matrix, release workflow with asset-count gate, README, MIT license"`
- [ ] **Step 4: Push to GitHub (user creates repo), tag `v0.1.0-alpha`, watch release workflow produce 6 binaries**

---

## Post-plan: real-machine validation checklist (release gate, from spec §9.4)

Not tasks — a manual gate before calling v1 done, run on the actual Loki machine:
- ☐ Windows slash-cwd `claude --resume` works on a pulled session
- ☐ checkpoint/rewind works when `trackedFileBackups` keys are slash-form
- ☐ plugins reinstall after pull
- ☐ identical behavior from Git Bash and PowerShell (`init`/`push`/`pull` each)
- ☐ `.claude.json` mcp merge leaves login intact
- ☐ Windows reserved-name file inside skills/ → pull skips + reports
- ☐ MAX_PATH-exceeding path handled via `\\?\`
- If slash-cwd resume FAILS → implement spec §5 plan B (JSON-string-scoped backslash re-escaping in pull-resolve; the tokenizer already provides the machinery) and re-run this checklist.

## Deviation from spec (intentional, documented)

### Deviations from plan discovered during implementation

- **Task 2 test literal**: the plan's `handles_escapes_and_unicode` test used `br#"...한글..."#` — Rust forbids non-ASCII in raw *byte* string literals (compile error). Replaced with the byte-identical `r#"..."#.as_bytes()`. Semantics unchanged.
- **Task 6 age Identity construction**: age 0.10 has no public raw-scalar constructor (`Identity` only offers `generate()` and bech32 `FromStr`; verified in crate source). Added the `bech32 = "0.9"` dependency (already in age's own tree) to encode the clamped scalar as `age-secret-key-…` and parse it. Derivation itself is exactly as planned (Argon2id 64B split).
- **Task 11 deps/config**: added `getrandom = "0.2"` (already in age's tree) — the plan's dependency list had no RNG for the random 32-byte salt. Added `Config.passphrase_env: Option<String>` (serde-default) so non-interactive commands know which env var carries the passphrase; the plan's `--passphrase-env` init flag implies persisting it.
- **Task 12 hash semantics**: `plaintext_hash` (manifest) and the `state.json` hashes are defined as sha256 of the **portable payload** (the plaintext that gets sealed), not of the raw local bytes. Raw bytes legitimately differ across devices (paths are rewritten), so raw-byte hashes would make every pulled file look "changed" forever and ping-pong no-op syncs between devices; the portable payload is byte-identical on both devices whenever content is in sync (guaranteed by the pull reverse-verify gate). Spec/plan wording ("평문 해시") is preserved — the portable payload *is* the plaintext of the sealed object.
- **Task 2 proptest oracle**: reproduced failure `minimal failing input: k = "z"`. `json!({k: v, "z": [...]})` collapses to one key when `k == "z"`, so the hardcoded expectation `[k, v, "z", v]` contradicts the oracle's own statement ("the strings serde sees"). Fixed with `prop_assume!(k != "z")` (degenerate-input exclusion, not a weakening — the lexer output was correct). Additionally, default `serde_json` sorts keys (BTreeMap), breaking the expected document order whenever `k > "z"`; enabled the `preserve_order` feature at Task 2 instead of Task 10 (Task 10 mandates it anyway).

### Deviation from spec

- Spec §5/§6 records per-span `PathShape` for verify-resolve. This plan records `SpanRecord { original: String }` instead: shape alone cannot restore case variants byte-exactly (`c:\users\loki` vs canonical), so invariant A would be unachievable. Recording the original span text is strictly stronger and keeps invariant B as the semantic check. `PathShape` remains available as a derived classification if needed later; the spec's intent (dual resolve, C1 fix) is preserved.

## Self-review notes (already applied)

- Spec coverage: §3 layouts→T8/T11; §4 inventory→T8/T10; §5 transform→T2–T5; §6 modules→T1–T10; §7 flows→T11–T13; §8 errors→exit codes in T11/T12; §9 tests/CI→T14/T15; §10/§11 → README (T15) + parking lot untouched. Gap check: spec §7 "미리보기 후 승인" for first pull on existing data — covered by `--dry-run` + backup in T12 (interactive preview deferred: YAGNI for the two-device owner; documented in README).
- Type consistency: `PathMapper`/`TokenMapping` (T1) consumed by T3/T4/T5; `SpanRecord` (T3) in T5 gate; `EntryMode` (T7) drives T12 skip-resolve; `TestEnv` (T11) reused T12/T13.
- Snapshot mode (`HistoryMode::Snapshot`) is config-modeled (T8) but only `Keep` is exercised in v1 flows — matches spec ("config 옵션"), wire = amend+force-push variant of push, deferred to v2 unless trivially added during T11.
