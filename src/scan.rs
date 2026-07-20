use crate::config::Config;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const ALLOWLIST: &[&str] = &[
    "projects",
    "history.jsonl",
    "file-history",
    "tasks",
    "todos",
    "plans",
    "settings.json",
    "settings.local.json",
    "CLAUDE.md",
    "keybindings.json",
    "agents",
    "skills",
    "commands",
    "rules",
    "workflows",
];

pub const EXCLUDED: &[&str] = &[
    "ide",
    "session-env",
    "sessions",
    "shell-snapshots",
    "chrome",
    "cache",
    "paste-cache",
    "debug",
    "downloads",
    "backups",
    "channels",
    "stats-cache.json",
    "mcp-needs-auth-cache.json",
    ".credentials.json",
    "plugins",
    ".claude.json",
    // machine-local state newer Claude Code versions create (server-fetched
    // caches and daemon/telemetry state — each machine refetches its own)
    "daemon",
    "telemetry",
    "policy-limits.json",
    "remote-settings.json",
    "vscode-claude-status-cache.json",
];

pub struct ScanResult {
    pub files: Vec<(String /*rel*/, PathBuf)>,
    pub unknown: Vec<String>,
}

/// Allowlist walk of the claude dir. Rel paths use '/' separators on all OSes.
/// Unknown = top-level entries in neither list nor `removed_paths`.
pub fn scan(claude_dir: &Path, cfg: &Config) -> anyhow::Result<ScanResult> {
    let mut files = Vec::new();
    let mut unknown = Vec::new();
    if !claude_dir.is_dir() {
        return Ok(ScanResult { files, unknown });
    }
    for entry in std::fs::read_dir(claude_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let removed = cfg.removed_paths.contains(&name);
        // same predicate pull and status consult — collection and
        // application can no longer disagree about membership
        let allowed = is_synced_rel(&name, cfg);
        if allowed {
            if ft.is_dir() {
                walk(&entry.path(), &name, &mut files)?;
            } else {
                files.push((name.clone(), entry.path()));
            }
        } else if !EXCLUDED.contains(&name.as_str())
            && !removed
            && !name.starts_with(".last-")
            && name != ".xsync-backups"
            && name != ".DS_Store"
            && !is_conflict_copy(&name)
        {
            unknown.push(name);
        }
    }
    files.sort();
    unknown.sort();
    Ok(ScanResult { files, unknown })
}

/// Conflict copies written by pull stay strictly machine-local: syncing them
/// would spray every device with each other's superseded versions.
pub fn is_conflict_copy(name: &str) -> bool {
    name.contains(".xsync-conflict.")
}

/// Machine-local by construction — no config can make these sync.
/// Works on rel and portable paths alike (only inspects mapper-independent
/// components: conflict-copy names and the plugins root).
pub fn is_never_synced_rel(rel: &str) -> bool {
    rel.split('/').any(is_conflict_copy)
        || rel
            .strip_prefix("plugins/")
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Is this path in THIS device's sync set? The single source of truth shared
/// by collection (push/scan), application (pull), and accounting (status):
/// a device must never receive remote content for a path it would not itself
/// collect. Accepts rel or portable form (the top segment and plugins root
/// are identical in both). Precedence: never-synced > removed_paths >
/// extra_paths (explicit opt-in wins, including a wholesale `plugins`) >
/// plugins root-manifest rule > ALLOWLIST. The mcp synthetic portable is
/// handled by its callers, not here.
pub fn is_synced_rel(rel: &str, cfg: &Config) -> bool {
    if is_never_synced_rel(rel) {
        return false;
    }
    let top = rel.split('/').next().unwrap_or(rel);
    if cfg.removed_paths.iter().any(|r| r == top) {
        return false;
    }
    if cfg.extra_paths.iter().any(|e| e == top) {
        return true;
    }
    if let Some(rest) = rel.strip_prefix("plugins/") {
        // only root-level manifest files sync; dirs are machine-built
        return !rest.contains('/');
    }
    ALLOWLIST.contains(&top)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type()?;
        let rel = format!("{prefix}/{name}");
        if ft.is_symlink() || name == ".DS_Store" || is_never_synced_rel(&rel) {
            continue;
        }
        if ft.is_dir() {
            walk(&entry.path(), &rel, out)?;
        } else {
            out.push((rel, entry.path()));
        }
    }
    Ok(())
}

pub fn sha256_file(p: &Path) -> anyhow::Result<String> {
    let data = std::fs::read(p)?;
    Ok(hex::encode(Sha256::digest(&data)))
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    #[test]
    fn scan_allowlisted_only_with_slash_rels_and_unknown_detection() {
        let td = tempfile::tempdir().unwrap();
        fs::create_dir_all(td.path().join("projects/a")).unwrap();
        fs::write(td.path().join("projects/a/s.jsonl"), b"{}\n").unwrap();
        fs::write(td.path().join("settings.json"), b"{}").unwrap();
        fs::create_dir_all(td.path().join("ide")).unwrap();
        fs::write(td.path().join("ide/x"), b"x").unwrap();
        fs::create_dir_all(td.path().join("weird-new-dir")).unwrap();
        fs::write(td.path().join("weird-new-dir/f"), b"f").unwrap();
        // macOS noise must be silently ignored at every level
        fs::write(td.path().join(".DS_Store"), b"junk").unwrap();
        fs::write(td.path().join("projects/a/.DS_Store"), b"junk").unwrap();

        let cfg = Config::default();
        let r = scan(td.path(), &cfg).unwrap();
        let rels: Vec<&str> = r.files.iter().map(|(rel, _)| rel.as_str()).collect();
        assert_eq!(rels, vec!["projects/a/s.jsonl", "settings.json"]);
        assert_eq!(r.unknown, vec!["weird-new-dir".to_string()]);
    }

    #[test]
    fn sync_set_predicates() {
        assert!(is_never_synced_rel("plugins/.last_inuse_sweep"));
        assert!(is_never_synced_rel("projects/a/s.jsonl.xsync-conflict.123"));
        assert!(!is_never_synced_rel("plugins/config.json"));

        let mut cfg = Config::default();
        assert!(is_synced_rel("settings.json", &cfg));
        assert!(is_synced_rel("projects/${HOME}-ws-app/s.jsonl", &cfg));
        assert!(is_synced_rel("plugins/config.json", &cfg));
        assert!(!is_synced_rel("plugins/.last_inuse_sweep", &cfg));
        assert!(!is_synced_rel("plugins/cache/x.node", &cfg));
        assert!(!is_synced_rel("daemon/marker.txt", &cfg));
        cfg.extra_paths.push("daemon".into());
        assert!(is_synced_rel("daemon/marker.txt", &cfg));
        cfg.removed_paths.push("todos".into());
        assert!(!is_synced_rel("todos/t.json", &cfg));

        // wholesale plugins opt-in: nested files sync, structural
        // machine-local markers still never do
        let mut whole = Config::default();
        whole.extra_paths.push("plugins".into());
        assert!(is_synced_rel("plugins/repos/foo/manifest.json", &whole));
        assert!(!is_synced_rel("plugins/.last_inuse_sweep", &whole));
    }

    #[test]
    fn known_machine_local_entries_are_silently_excluded() {
        let td = tempfile::tempdir().unwrap();
        fs::write(td.path().join("settings.json"), b"{}").unwrap();
        // machine-local state newer Claude Code versions create — never
        // synced, and known well enough that warning about it is noise
        for f in [
            "policy-limits.json",
            "remote-settings.json",
            "vscode-claude-status-cache.json",
        ] {
            fs::write(td.path().join(f), b"{}").unwrap();
        }
        for d in ["daemon", "telemetry"] {
            fs::create_dir_all(td.path().join(d)).unwrap();
            fs::write(td.path().join(d).join("x"), b"x").unwrap();
        }
        // a genuinely unknown entry must still be reported
        fs::create_dir_all(td.path().join("brand-new-thing")).unwrap();
        fs::write(td.path().join("brand-new-thing/f"), b"f").unwrap();

        let r = scan(td.path(), &Config::default()).unwrap();
        let rels: Vec<&str> = r.files.iter().map(|(rel, _)| rel.as_str()).collect();
        assert_eq!(rels, vec!["settings.json"]);
        assert_eq!(r.unknown, vec!["brand-new-thing".to_string()]);
    }

    #[test]
    fn sha256_file_hex() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f");
        fs::write(&p, b"hello").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
