use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub enum HistoryMode {
    #[default]
    Keep,
    Snapshot,
}

/// `~/.claude-xsync/config.toml`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub remote: String,
    pub device: String,
    #[serde(default)]
    pub path_map: BTreeMap<String, String>, // local path → TOKEN
    #[serde(default)]
    pub extra_paths: Vec<String>,
    #[serde(default)]
    pub removed_paths: Vec<String>,
    #[serde(default)]
    pub history_mode: HistoryMode, // Keep | Snapshot
    /// Env var name holding the passphrase (set by `init --passphrase-env`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase_env: Option<String>,
}

pub fn claude_dir() -> PathBuf {
    std::env::var_os("XSYNC_CLAUDE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".claude"))
}

pub fn xsync_dir() -> PathBuf {
    std::env::var_os("XSYNC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".claude-xsync"))
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("XSYNC_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("cannot determine home directory"))
}

pub fn config_path() -> PathBuf {
    xsync_dir().join("config.toml")
}

pub fn load_config() -> anyhow::Result<Config> {
    let p = config_path();
    let text = std::fs::read_to_string(&p).map_err(|e| {
        anyhow::anyhow!(
            "cannot read {} — run `claude-xsync init` first ({e})",
            p.display()
        )
    })?;
    Ok(toml::from_str(&text)?)
}

pub fn save_config(cfg: &Config) -> anyhow::Result<()> {
    let p = config_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, toml::to_string_pretty(cfg)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_roundtrip_defaults() {
        let cfg = Config {
            remote: "git@github.com:u/r.git".into(),
            device: "mac".into(),
            ..Config::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.remote, "git@github.com:u/r.git");
        assert_eq!(back.device, "mac");
        assert!(back.path_map.is_empty());
        assert!(matches!(back.history_mode, HistoryMode::Keep));
    }
}
