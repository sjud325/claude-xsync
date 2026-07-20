use std::io::Write;
use std::path::{Path, PathBuf};

/// Write via temp file in the same directory + rename; creates parents.
pub fn atomic_write(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.flush()?;
    tmp.persist(long_path(path))?;
    Ok(())
}

/// Copy each existing rel into `<claude_dir>.backup.<stamp>/<rel>`.
pub fn backup_files(claude_dir: &Path, rels: &[String], stamp: &str) -> anyhow::Result<PathBuf> {
    let name = claude_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| ".claude".into());
    let backup = claude_dir.with_file_name(format!("{name}.backup.{stamp}"));
    std::fs::create_dir_all(&backup)?;
    for rel in rels {
        let src = claude_dir.join(rel);
        if src.is_file() {
            let dst = backup.join(rel);
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(backup)
}

/// Delete all but the newest `keep` pull-backup dirs (`<claude>.backup.<ts>`,
/// numeric-stamp sort). Only exact matches are touched. Dirs stamped after
/// `now_stamp` are never candidates: a clock that once ran ahead must not
/// make the backup this very pull just created the "oldest" one (review
/// v0.1.16 finding B). Returns pruned names.
pub fn prune_backups(
    claude_dir: &Path,
    keep: usize,
    now_stamp: u64,
) -> anyhow::Result<Vec<String>> {
    let name = claude_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| ".claude".into());
    let prefix = format!("{name}.backup.");
    let Some(parent) = claude_dir.parent() else {
        return Ok(Vec::new());
    };
    let mut stamped: Vec<(u64, String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let Some(stamp) = dir_name.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(ts) = stamp.parse::<u64>() else {
            continue;
        };
        if ts > now_stamp {
            continue; // future-stamped (clock skew) — leave alone
        }
        stamped.push((ts, dir_name, entry.path()));
    }
    stamped.sort_by_key(|s| std::cmp::Reverse(s.0)); // newest first
    let mut pruned = Vec::new();
    for (_, dir_name, path) in stamped.into_iter().skip(keep) {
        std::fs::remove_dir_all(&path)?;
        pruned.push(dir_name);
    }
    Ok(pruned)
}

/// Set a file's modification time (unix secs).
pub fn set_mtime(path: &Path, unix_secs: u64) -> anyhow::Result<()> {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(long_path(path))?;
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs))?;
    Ok(())
}

/// Windows: prefix `\\?\` when the path grows past MAX_PATH territory; no-op elsewhere.
pub fn long_path(p: &Path) -> PathBuf {
    if cfg!(windows) {
        let s = p.to_string_lossy();
        if s.len() > 240 && !s.starts_with(r"\\?\") {
            return PathBuf::from(format!(r"\\?\{s}"));
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn atomic_write_creates_nested_parents() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("a/b/c/file.json");
        atomic_write(&p, b"data").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"data");
        // overwrite in place
        atomic_write(&p, b"data2").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"data2");
    }

    #[test]
    fn backup_copies_only_existing_files() {
        let td = tempfile::tempdir().unwrap();
        let claude = td.path().join(".claude");
        fs::create_dir_all(claude.join("projects/a")).unwrap();
        fs::write(claude.join("settings.json"), b"{}").unwrap();
        fs::write(claude.join("projects/a/s.jsonl"), b"{}\n").unwrap();
        let rels = vec![
            "settings.json".to_string(),
            "projects/a/s.jsonl".to_string(),
            "missing.json".to_string(),
        ];
        let backup = backup_files(&claude, &rels, "20260714").unwrap();
        assert!(backup.to_string_lossy().contains(".claude.backup.20260714"));
        assert_eq!(fs::read(backup.join("settings.json")).unwrap(), b"{}");
        assert_eq!(
            fs::read(backup.join("projects/a/s.jsonl")).unwrap(),
            b"{}\n"
        );
        assert!(!backup.join("missing.json").exists());
    }

    #[test]
    fn prune_backups_keeps_newest_n_by_numeric_stamp() {
        let td = tempfile::tempdir().unwrap();
        let claude = td.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        // numeric sort, not lexicographic: 100 > 99
        for stamp in ["99", "100", "300"] {
            let d = td.path().join(format!(".claude.backup.{stamp}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("f"), b"x").unwrap();
        }
        // non-matching neighbors must never be touched
        fs::create_dir_all(td.path().join(".claude.backup.notanum")).unwrap();
        fs::create_dir_all(td.path().join(".claude.backupX")).unwrap();
        fs::write(td.path().join(".claude.backup.50"), b"a file, not a dir").unwrap();

        let pruned = prune_backups(&claude, 2, 400).unwrap();
        assert_eq!(pruned, vec![".claude.backup.99".to_string()]);
        assert!(!td.path().join(".claude.backup.99").exists());
        assert!(td.path().join(".claude.backup.100").exists());
        assert!(td.path().join(".claude.backup.300").exists());
        assert!(td.path().join(".claude.backup.notanum").exists());
        assert!(td.path().join(".claude.backupX").exists());
        assert!(td.path().join(".claude.backup.50").is_file());

        // under the limit → nothing to do
        assert!(prune_backups(&claude, 2, 400).unwrap().is_empty());
    }

    #[test]
    fn prune_backups_protects_current_and_future_stamps() {
        let td = tempfile::tempdir().unwrap();
        let claude = td.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        // 300 is future-stamped garbage from a clock that once ran ahead;
        // 200 is the backup the current pull (now = 200) just created
        for stamp in ["100", "150", "200", "300"] {
            fs::create_dir_all(td.path().join(format!(".claude.backup.{stamp}"))).unwrap();
        }
        let pruned = prune_backups(&claude, 2, 200).unwrap();
        assert_eq!(pruned, vec![".claude.backup.100".to_string()]);
        assert!(
            td.path().join(".claude.backup.200").exists(),
            "the just-created backup must never be the prune victim"
        );
        assert!(td.path().join(".claude.backup.150").exists());
        assert!(
            td.path().join(".claude.backup.300").exists(),
            "future-stamped dirs are left alone"
        );
    }

    #[test]
    fn long_path_noop_on_unix() {
        let p = std::path::Path::new("/tmp/x");
        assert_eq!(long_path(p), p.to_path_buf());
    }
}
