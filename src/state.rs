use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `~/.claude-xsync/state.json`
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub files: BTreeMap<String, String>, // portable path → plaintext sha256 hex
    pub last_synced_commit: Option<String>,
}

pub fn state_path() -> PathBuf {
    crate::config::xsync_dir().join("state.json")
}

pub fn load_state() -> State {
    std::fs::read(state_path())
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

pub fn save_state(state: &State) -> anyhow::Result<()> {
    // atomic: a crash mid-write must never leave a truncated state.json
    crate::fsx::atomic_write(&state_path(), &serde_json::to_vec_pretty(state)?)
}

/// Per-file immediate write — interrupted pulls resume exactly where they stopped.
pub fn upsert_and_save(state: &mut State, portable: &str, hash: &str) -> anyhow::Result<()> {
    state.files.insert(portable.to_string(), hash.to_string());
    save_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_writes_file_immediately() {
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("XSYNC_DIR", td.path());
        let mut st = State::default();
        upsert_and_save(&mut st, "settings.json", "abc123").unwrap();
        // read back from disk — per-file immediate persistence is the contract
        let raw = std::fs::read_to_string(td.path().join("state.json")).unwrap();
        assert!(raw.contains("settings.json"));
        assert!(raw.contains("abc123"));
        let loaded = load_state();
        assert_eq!(loaded.files["settings.json"], "abc123");
        std::env::remove_var("XSYNC_DIR");
    }
}
