//! Command orchestration only — no transform/crypto logic lives here.
pub mod init;
pub mod push;

use crate::config::Config;
use crate::crypto::{self, Keys};
use crate::manifest::Manifest;
use std::path::{Path, PathBuf};

pub fn repo_dir() -> PathBuf {
    crate::config::xsync_dir().join("repo")
}

pub fn read_salt(repo: &Path) -> anyhow::Result<[u8; 32]> {
    let raw = std::fs::read_to_string(repo.join("salt"))
        .map_err(|e| anyhow::anyhow!("missing repo salt — run init first ({e})"))?;
    let bytes = hex::decode(raw.trim())
        .map_err(|e| anyhow::anyhow!("corrupt salt file: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("corrupt salt file: expected 32 bytes"))
}

pub fn load_keys(cfg: &Config, repo: &Path) -> anyhow::Result<Keys> {
    let env_name = cfg.passphrase_env.as_deref().unwrap_or("XSYNC_PASSPHRASE");
    let pass = std::env::var(env_name)
        .map_err(|_| anyhow::anyhow!("passphrase env var {env_name} is not set"))?;
    let salt = read_salt(repo)?;
    Ok(crypto::derive(&pass, &salt))
}

pub fn read_manifest(repo: &Path, keys: &Keys) -> anyhow::Result<Option<Manifest>> {
    let p = repo.join("manifest.age");
    if !p.exists() {
        return Ok(None);
    }
    let sealed = std::fs::read(&p)?;
    let plain = crypto::open(&sealed, &keys.identity)
        .map_err(|e| anyhow::anyhow!("cannot decrypt manifest — wrong passphrase? ({e})"))?;
    Ok(Some(serde_json::from_slice(&plain)?))
}

pub fn write_manifest(repo: &Path, keys: &Keys, m: &Manifest) -> anyhow::Result<()> {
    let plain = serde_json::to_vec(m)?;
    let sealed = crypto::seal(&plain, &keys.recipient);
    crate::fsx::atomic_write(&repo.join("manifest.age"), &sealed)
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fixed end-of-command summary line (spec §8).
pub struct Summary {
    pub synced: usize,
    pub verbatim: usize,
    pub verbatim_reasons: Vec<String>,
    pub skipped: usize,
    pub conflicts: usize,
}

impl Summary {
    pub fn new() -> Summary {
        Summary { synced: 0, verbatim: 0, verbatim_reasons: Vec::new(), skipped: 0, conflicts: 0 }
    }
    pub fn print(&self) {
        let reasons = if self.verbatim_reasons.is_empty() {
            String::new()
        } else {
            let mut uniq = self.verbatim_reasons.clone();
            uniq.sort();
            uniq.dedup();
            format!(" ({})", uniq.join("; "))
        };
        println!(
            "✓ {} synced · ⚠ {} verbatim{} · ✗ {} skipped · ⚡ {} conflicts",
            self.synced, self.verbatim, reasons, self.skipped, self.conflicts
        );
    }
    /// Exit code per spec §8: 1 = completed with warnings, 0 = clean.
    pub fn exit_code(&self) -> i32 {
        if self.verbatim > 0 || self.skipped > 0 { 1 } else { 0 }
    }
}

impl Default for Summary {
    fn default() -> Self {
        Self::new()
    }
}

/// rel path → portable path (projects/<key> dir segment tokenized).
pub fn rel_to_portable(rel: &str, m: &crate::mapper::PathMapper) -> String {
    let mut parts: Vec<String> = rel.split('/').map(|s| s.to_string()).collect();
    if parts.len() >= 2 && parts[0] == "projects" {
        parts[1] = crate::transform::dirkey::key_to_portable(&parts[1], m);
    }
    parts.join("/")
}

/// portable path → local rel path; strict on dir-key tokens.
pub fn portable_to_rel(
    portable: &str,
    m: &crate::mapper::PathMapper,
) -> Result<String, crate::transform::dirkey::UnmappedToken> {
    let mut parts: Vec<String> = portable.split('/').map(|s| s.to_string()).collect();
    if parts.len() >= 2 && parts[0] == "projects" {
        parts[1] = crate::transform::dirkey::portable_to_key(&parts[1], m)?;
    }
    Ok(parts.join("/"))
}

/// Abort (exit 2) when a live Claude Code instance is detected, unless forced.
pub fn guard_running(claude_dir: &Path, force: bool) -> anyhow::Result<()> {
    let pids = crate::procguard::claude_running(claude_dir);
    if !pids.is_empty() && !force {
        anyhow::bail!(
            "Claude Code appears to be running (pids {pids:?}) — close it or re-run with --force"
        );
    }
    Ok(())
}
