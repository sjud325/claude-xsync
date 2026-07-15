//! Managed multi-device note in `~/.claude/CLAUDE.md`.
//!
//! Sessions resumed on the other machine reliably trip over machine-local
//! state that sync can't (and shouldn't) carry: /tmp scratchpad paths from
//! earlier turns, untracked files in project working trees. This note primes
//! the model to re-check the current environment instead of trusting
//! remembered paths. Inserted as a marker-delimited block so it can be
//! detected (idempotent) and removed by the user; deliberately contains no
//! literal home paths — the path transform would localize them on the peer
//! and corrupt the text.

use crate::config;
use std::path::Path;

pub const BEGIN: &str = "<!-- claude-xsync:multi-device:begin -->";
pub const END: &str = "<!-- claude-xsync:multi-device:end -->";

fn block() -> String {
    format!(
        "{BEGIN}\n\
         ## Multi-device sessions (claude-xsync)\n\
         \n\
         This `~/.claude` is synced across machines with different OSes,\n\
         usernames, and path layouts. When resuming a session that was\n\
         started on another machine:\n\
         \n\
         - Trust the current pwd/environment over paths remembered from\n\
         \x20 earlier turns.\n\
         - Temp/scratchpad paths from earlier turns (`/tmp`, `/private/tmp`,\n\
         \x20 `%TEMP%`, ...) are machine-specific and will NOT exist here —\n\
         \x20 use the current session's scratchpad or `mktemp` instead.\n\
         - Untracked files in project working trees (scratch dirs, local\n\
         \x20 configs) did not transfer between machines; only committed\n\
         \x20 files exist here.\n\
         {END}\n"
    )
}

/// Append the managed block to `<claude_dir>/CLAUDE.md` unless it is already
/// present. Returns whether anything was written.
pub fn ensure_note(claude_dir: &Path) -> anyhow::Result<bool> {
    let path = claude_dir.join("CLAUDE.md");
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if existing.contains(BEGIN) {
        return Ok(false);
    }
    std::fs::create_dir_all(claude_dir)?;
    let mut out = existing;
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&block());
    crate::fsx::atomic_write(&path, out.as_bytes())?;
    Ok(true)
}

pub fn run_claude_md() -> anyhow::Result<i32> {
    let dir = config::claude_dir();
    if ensure_note(&dir)? {
        println!(
            "added the multi-device note to {} — push to propagate it to your other devices",
            dir.join("CLAUDE.md").display()
        );
    } else {
        println!("multi-device note already present — nothing to do");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_has_no_absolute_paths_and_balanced_markers() {
        let b = block();
        assert!(b.starts_with(BEGIN));
        assert!(b.trim_end().ends_with(END));
        // nothing that the home-path transform could rewrite
        assert!(!b.contains("/Users/"));
        assert!(!b.contains("C:\\"));
    }

    #[test]
    fn ensure_note_creates_appends_and_is_idempotent() {
        let td = tempfile::tempdir().unwrap();
        assert!(ensure_note(td.path()).unwrap());
        let first = std::fs::read_to_string(td.path().join("CLAUDE.md")).unwrap();
        assert!(first.contains(BEGIN));
        assert!(!ensure_note(td.path()).unwrap());
        let second = std::fs::read_to_string(td.path().join("CLAUDE.md")).unwrap();
        assert_eq!(first, second);

        // appends after existing content, preserving it
        let td2 = tempfile::tempdir().unwrap();
        std::fs::write(td2.path().join("CLAUDE.md"), "# rules").unwrap();
        assert!(ensure_note(td2.path()).unwrap());
        let s = std::fs::read_to_string(td2.path().join("CLAUDE.md")).unwrap();
        assert!(s.starts_with("# rules\n\n"));
        assert!(s.contains(BEGIN));
    }
}
