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
        let allowed =
            (ALLOWLIST.contains(&name.as_str()) || cfg.extra_paths.contains(&name)) && !removed;
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
        {
            unknown.push(name);
        }
    }
    files.sort();
    unknown.sort();
    Ok(ScanResult { files, unknown })
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let rel = format!("{prefix}/{name}");
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

        let cfg = Config::default();
        let r = scan(td.path(), &cfg).unwrap();
        let rels: Vec<&str> = r.files.iter().map(|(rel, _)| rel.as_str()).collect();
        assert_eq!(rels, vec!["projects/a/s.jsonl", "settings.json"]);
        assert_eq!(r.unknown, vec!["weird-new-dir".to_string()]);
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
