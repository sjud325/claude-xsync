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
         - Installed tools and OS commands differ between machines: a\n\
         \x20 command that worked in earlier turns (`jq`, `brew`, `pbcopy`,\n\
         \x20 ...) may not exist here. Check with `command -v` before\n\
         \x20 relying on it and prefer the local equivalent.\n\
         {END}\n"
    )
}

#[derive(Debug, PartialEq)]
pub enum NoteAction {
    Added,
    Updated,
    Unchanged,
}

/// Ensure `<claude_dir>/CLAUDE.md` carries the current managed block:
/// append it when absent, refresh the text between the markers when an
/// older version is present. Everything outside the markers is preserved
/// byte-for-byte. Deleting the block opts out durably in practice because
/// nothing calls this automatically after init — only an explicit
/// `claude-md` run re-adds it.
pub fn ensure_note(claude_dir: &Path) -> anyhow::Result<NoteAction> {
    let path = claude_dir.join("CLAUDE.md");
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if let Some(start) = existing.find(BEGIN) {
        let tail = &existing[start..];
        let Some(end_rel) = tail.find(END) else {
            anyhow::bail!(
                "CLAUDE.md has a begin marker but no end marker — fix or remove the block manually"
            );
        };
        let span = &existing[start..start + end_rel + END.len()];
        let fresh = block();
        let fresh_span = fresh.trim_end_matches('\n');
        if span == fresh_span {
            return Ok(NoteAction::Unchanged);
        }
        let updated = existing.replacen(span, fresh_span, 1);
        crate::fsx::atomic_write(&path, updated.as_bytes())?;
        return Ok(NoteAction::Updated);
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
    Ok(NoteAction::Added)
}

pub fn run_claude_md() -> anyhow::Result<i32> {
    let dir = config::claude_dir();
    let path = dir.join("CLAUDE.md");
    match ensure_note(&dir)? {
        NoteAction::Added => println!(
            "added the multi-device note to {} — push to propagate it to your other devices",
            path.display()
        ),
        NoteAction::Updated => println!(
            "updated the multi-device note in {} to the current version — push to propagate it",
            path.display()
        ),
        NoteAction::Unchanged => println!("multi-device note already up to date — nothing to do"),
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
        assert_eq!(ensure_note(td.path()).unwrap(), NoteAction::Added);
        let first = std::fs::read_to_string(td.path().join("CLAUDE.md")).unwrap();
        assert!(first.contains(BEGIN));
        assert_eq!(ensure_note(td.path()).unwrap(), NoteAction::Unchanged);
        let second = std::fs::read_to_string(td.path().join("CLAUDE.md")).unwrap();
        assert_eq!(first, second);

        // appends after existing content, preserving it
        let td2 = tempfile::tempdir().unwrap();
        std::fs::write(td2.path().join("CLAUDE.md"), "# rules").unwrap();
        assert_eq!(ensure_note(td2.path()).unwrap(), NoteAction::Added);
        let s = std::fs::read_to_string(td2.path().join("CLAUDE.md")).unwrap();
        assert!(s.starts_with("# rules\n\n"));
        assert!(s.contains(BEGIN));
    }

    #[test]
    fn stale_block_is_refreshed_in_place_preserving_surroundings() {
        let td = tempfile::tempdir().unwrap();
        let stale = format!("# top\n\n{BEGIN}\nold note text\n{END}\n\n# bottom\n");
        std::fs::write(td.path().join("CLAUDE.md"), &stale).unwrap();
        assert_eq!(ensure_note(td.path()).unwrap(), NoteAction::Updated);
        let s = std::fs::read_to_string(td.path().join("CLAUDE.md")).unwrap();
        assert!(s.starts_with("# top\n\n"), "{s}");
        assert!(s.ends_with("\n\n# bottom\n"), "{s}");
        assert!(!s.contains("old note text"));
        assert!(s.contains("command -v"), "new tools bullet missing: {s}");
        assert_eq!(ensure_note(td.path()).unwrap(), NoteAction::Unchanged);
    }
}
