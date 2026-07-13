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
    fn long_path_noop_on_unix() {
        let p = std::path::Path::new("/tmp/x");
        assert_eq!(long_path(p), p.to_path_buf());
    }
}
