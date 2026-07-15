//! `app-index`: backfill Claude **desktop app** session-index entries for
//! synced sessions. The app's session list is sourced exclusively from its
//! private `local_*.json` index (claude-code-sessions/<account>/<org>/), so
//! sessions pulled into `~/.claude/projects` are invisible to it until an
//! index entry exists. This is opt-in (not part of `pull`) because the index
//! format is app-private and may change between app versions.
//!
//! Existing index entries are never modified or deleted — only missing ones
//! are added, cloned from the most recent native entry so platform-specific
//! fields carry over.

use anyhow::Context;
use std::collections::BTreeSet;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::config;

pub fn run_app_index(dry_run: bool) -> anyhow::Result<i32> {
    let sessions_dir = resolve_app_sessions_dir()?;

    // Scan the existing index: which cliSessionIds are already listed, and
    // the most recently active entry to use as a field template.
    let mut indexed: BTreeSet<String> = BTreeSet::new();
    let mut template: Option<(u64, serde_json::Value)> = None;
    if sessions_dir.is_dir() {
        for f in std::fs::read_dir(&sessions_dir)?.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if !(name.starts_with("local_") && name.ends_with(".json")) {
                continue;
            }
            let Ok(bytes) = std::fs::read(f.path()) else {
                continue;
            };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            if let Some(id) = v.get("cliSessionId").and_then(|x| x.as_str()) {
                indexed.insert(id.to_string());
            }
            let act = v
                .get("lastActivityAt")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            if template.as_ref().is_none_or(|(a, _)| act >= *a) {
                template = Some((act, v));
            }
        }
    }

    // Top-level session transcripts only: projects/<proj>/<uuid>.jsonl.
    // Subagent/tool transcripts live in subdirectories and must never be
    // listed as sessions.
    let projects = config::claude_dir().join("projects");
    let mut to_index: Vec<PathBuf> = Vec::new();
    let mut already = 0usize;
    if projects.is_dir() {
        for proj in std::fs::read_dir(&projects)?.flatten() {
            if !proj.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(proj.path())?.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !is_uuid_name(stem) {
                    continue;
                }
                if indexed.contains(stem) {
                    already += 1;
                } else {
                    to_index.push(p);
                }
            }
        }
    }

    let total = to_index.len();
    let mut created = 0usize;
    let mut skipped = 0usize;
    for (i, path) in to_index.iter().enumerate() {
        if i > 0 && i % 200 == 0 {
            println!("  … scanned {i}/{total} sessions");
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let meta = match extract_session_meta(path) {
            Ok(Some(m)) => m,
            Ok(None) => {
                skipped += 1;
                continue;
            }
            Err(e) => {
                eprintln!("  ! skipping {stem}: {e}");
                skipped += 1;
                continue;
            }
        };
        if dry_run {
            println!("  would index: {} ({stem})", meta.title);
            created += 1;
            continue;
        }
        let uuid = uuid_v4()?;
        let entry = build_entry(template.as_ref().map(|(_, v)| v), stem, &meta, &uuid);
        std::fs::create_dir_all(&sessions_dir)?;
        crate::fsx::atomic_write(
            &sessions_dir.join(format!("local_{uuid}.json")),
            serde_json::to_string(&entry)?.as_bytes(),
        )?;
        created += 1;
    }

    if dry_run {
        println!("✓ would index {created} sessions ({already} already indexed, {skipped} skipped)");
    } else if created == 0 {
        println!(
            "✓ nothing to index — {already} sessions already in the app list ({skipped} skipped)"
        );
    } else {
        println!("✓ indexed {created} sessions into the Claude app list ({already} already indexed, {skipped} skipped)");
        println!("  restart the Claude desktop app to see them");
    }
    Ok(0)
}

struct SessionMeta {
    title: String,
    title_source: &'static str,
    created_ms: u64,
    last_ms: u64,
    cwd: String,
}

/// Stream a session .jsonl once: first/last record timestamps, cwd, and the
/// best available title (custom-title record > ai-title record > first user
/// message). Large lines (multi-MB tool results) are only substring-scanned,
/// never JSON-parsed, unless they carry a field we still need.
fn extract_session_meta(path: &Path) -> anyhow::Result<Option<SessionMeta>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut created: Option<u64> = None;
    let mut last: Option<u64> = None;
    let mut cwd: Option<String> = None;
    let mut title: Option<String> = None;
    let mut fallback_title: Option<String> = None;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if let Some(ts) = raw_str_field(&line, "timestamp").and_then(iso_to_epoch_ms) {
            created = created.or(Some(ts));
            last = Some(ts);
        }
        let need_parse = (cwd.is_none() && line.contains("\"cwd\":"))
            || line.contains("\"type\":\"custom-title\"")
            || (title.is_none() && line.contains("\"type\":\"ai-title\""))
            || (title.is_none() && fallback_title.is_none() && line.contains("\"type\":\"user\""));
        if !need_parse {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
        }
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => {
                if let Some(t) = v.get("customTitle").and_then(|x| x.as_str()) {
                    title = Some(t.to_string());
                }
            }
            Some("ai-title") if title.is_none() => {
                title = v
                    .get("aiTitle")
                    .or_else(|| v.get("title"))
                    .and_then(|x| x.as_str())
                    .map(str::to_string);
            }
            Some("user") if fallback_title.is_none() => {
                let text = match v.get("message").and_then(|m| m.get("content")) {
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(serde_json::Value::Array(a)) => a
                        .iter()
                        .find_map(|i| i.get("text").and_then(|t| t.as_str()).map(str::to_string)),
                    _ => None,
                };
                if let Some(t) = text {
                    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        fallback_title = Some(t.chars().take(60).collect());
                    }
                }
            }
            _ => {}
        }
    }
    let (Some(created_ms), Some(last_ms), Some(cwd)) = (created, last, cwd) else {
        return Ok(None); // empty or unrecognizable transcript
    };
    // titleSource "custom" pins the title so the app never regenerates it.
    let (title, title_source) = match title.or(fallback_title) {
        Some(t) => (t, "custom"),
        None => ("Untitled session".to_string(), "custom"),
    };
    Ok(Some(SessionMeta {
        title,
        title_source,
        created_ms,
        last_ms,
        cwd,
    }))
}

fn build_entry(
    template: Option<&serde_json::Value>,
    cli_session_id: &str,
    meta: &SessionMeta,
    uuid: &str,
) -> serde_json::Value {
    let mut obj = template
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    // the app stores cwd in the platform's native separator form
    let native_cwd = if cfg!(windows) {
        meta.cwd.replace('/', "\\")
    } else {
        meta.cwd.clone()
    };
    obj.insert(
        "sessionId".into(),
        serde_json::json!(format!("local_{uuid}")),
    );
    obj.insert("cliSessionId".into(), serde_json::json!(cli_session_id));
    obj.insert("cwd".into(), serde_json::json!(native_cwd));
    obj.insert("originCwd".into(), serde_json::json!(native_cwd));
    obj.insert("title".into(), serde_json::json!(meta.title));
    obj.insert("titleSource".into(), serde_json::json!(meta.title_source));
    obj.insert("createdAt".into(), serde_json::json!(meta.created_ms));
    obj.insert("lastActivityAt".into(), serde_json::json!(meta.last_ms));
    obj.insert("isArchived".into(), serde_json::json!(false));
    if obj.contains_key("lastFocusedAt") {
        obj.insert("lastFocusedAt".into(), serde_json::json!(meta.last_ms));
    }
    // session-specific leftovers inherited from the template must not leak
    if obj.contains_key("completedTurns") {
        obj.insert("completedTurns".into(), serde_json::json!(0));
    }
    for k in [
        "writtenBranches",
        "bridgeSessionIds",
        "alwaysAllowedReasons",
        "sessionPermissionUpdates",
    ] {
        if obj.contains_key(k) {
            obj.insert(k.into(), serde_json::json!([]));
        }
    }
    obj.remove("spawnSeed");
    serde_json::Value::Object(obj)
}

/// Locate the app's session-index directory:
/// `<app data>/Claude/claude-code-sessions/<account>/<org>/`. The account and
/// org UUIDs are discovered by finding the subdirectory that already holds
/// `local_*.json` entries (the app must have run at least once).
fn resolve_app_sessions_dir() -> anyhow::Result<PathBuf> {
    if let Some(d) = std::env::var_os("XSYNC_APP_SESSIONS_DIR") {
        return Ok(PathBuf::from(d));
    }
    let base = if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA").context("APPDATA is not set")?).join("Claude")
    } else if cfg!(target_os = "macos") {
        config::home_dir().join("Library/Application Support/Claude")
    } else {
        anyhow::bail!("app-index only supports the Claude desktop app on Windows and macOS");
    };
    let root = base.join("claude-code-sessions");
    let mut best: Option<(usize, PathBuf)> = None;
    if root.is_dir() {
        for acc in std::fs::read_dir(&root)?.flatten() {
            if !acc.path().is_dir() {
                continue;
            }
            for org in std::fs::read_dir(acc.path())?.flatten() {
                if !org.path().is_dir() {
                    continue;
                }
                let n = std::fs::read_dir(org.path())?
                    .flatten()
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        name.starts_with("local_") && name.ends_with(".json")
                    })
                    .count();
                if n > 0 && best.as_ref().is_none_or(|(b, _)| n > *b) {
                    best = Some((n, org.path()));
                }
            }
        }
    }
    best.map(|(_, p)| p).with_context(|| {
        format!(
            "no Claude app session index found under {} — open the Claude desktop app once, then retry",
            root.display()
        )
    })
}

fn is_uuid_name(stem: &str) -> bool {
    let b = stem.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Extract a raw string field from a JSONL line without parsing it. Only
/// sound for values that never contain escape sequences (ISO timestamps);
/// escaped occurrences inside message content can't match because their
/// quotes are backslash-prefixed.
fn raw_str_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = line.find(&pat)? + pat.len();
    let end = line[start..].find('"')? + start;
    Some(&line[start..end])
}

/// `2026-07-13T18:19:49.414Z` → unix epoch milliseconds. Hand-rolled to
/// avoid a chrono dependency; accepts only the UTC form Claude Code writes.
fn iso_to_epoch_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<u64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let ms = if b.len() > 20 && b[19] == b'.' {
        let frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let v = frac.parse::<u64>().ok()?;
        match frac.len() {
            1 => v * 100,
            2 => v * 10,
            3 => v,
            n => v / 10u64.pow(n as u32 - 3),
        }
    } else {
        0
    };
    let days = days_from_civil(y as i64, mo, d);
    if days < 0 {
        return None;
    }
    Some((days as u64 * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000 + ms)
}

/// Days from 1970-01-01 to y-m-d (proleptic Gregorian, Hinnant's algorithm).
fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn uuid_v4() -> anyhow::Result<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b)
        .map_err(|e| anyhow::anyhow!("cannot generate random uuid: {e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    let h = hex::encode(b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parse_known_values() {
        assert_eq!(
            iso_to_epoch_ms("2020-01-01T00:00:00.000Z"),
            Some(1_577_836_800_000)
        );
        assert_eq!(
            iso_to_epoch_ms("2020-01-02T03:04:05.678Z"),
            Some(1_577_934_245_678)
        );
        assert_eq!(iso_to_epoch_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            iso_to_epoch_ms("2026-07-13T18:19:49.414Z"),
            Some(1_783_966_789_414)
        );
        assert_eq!(iso_to_epoch_ms("not a date"), None);
        assert_eq!(iso_to_epoch_ms("2020-13-01T00:00:00Z"), None);
    }

    #[test]
    fn uuid_shape() {
        let u = uuid_v4().unwrap();
        assert!(is_uuid_name(&u), "{u}");
        assert_eq!(u.as_bytes()[14], b'4'); // version nibble
        let a = uuid_v4().unwrap();
        assert_ne!(u, a);
    }

    #[test]
    fn uuid_name_filter() {
        assert!(is_uuid_name("11ae1b5f-3755-4863-9dc2-b59847edd7aa"));
        assert!(!is_uuid_name("s"));
        assert!(!is_uuid_name("agent-abc"));
        assert!(!is_uuid_name("11ae1b5f-3755-4863-9dc2-b59847edd7aa.bak"));
    }

    #[test]
    fn raw_field_ignores_escaped_content() {
        let line = r#"{"content":"quoted \"timestamp\":\"1999-01-01T00:00:00Z\" text","timestamp":"2020-01-01T00:00:00.000Z"}"#;
        assert_eq!(
            raw_str_field(line, "timestamp"),
            Some("2020-01-01T00:00:00.000Z")
        );
    }
}
