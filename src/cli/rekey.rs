use crate::cli::{
    collect_locals, guard_running, load_keys, read_manifest, repo_dir, unix_now, write_manifest,
    MCP_PORTABLE,
};
use crate::config;
use crate::crypto::{self, object_name};
use crate::gitx::Git;
use crate::manifest::{chunk_paths, split_chunks, Entry, Manifest};
use crate::mapper::PathMapper;
use crate::state::{self, State};
use std::collections::BTreeMap;

/// Re-encrypt everything from local plaintext under a NEW passphrase + salt,
/// then squash history so nothing decryptable with the old key survives.
pub fn run_rekey(passphrase_env: String, force: bool) -> anyhow::Result<i32> {
    let cfg = config::load_config()?;
    let claude_dir = config::claude_dir();
    guard_running(&claude_dir, false)?;

    let new_pass = std::env::var(&passphrase_env)
        .map_err(|_| anyhow::anyhow!("new passphrase env var {passphrase_env} is not set"))?;

    let repo = repo_dir();
    let git = Git::clone_or_open(&cfg.remote, &repo)?;
    git.fetch()?;
    // Guard (review C2): rekey rebuilds the manifest from LOCAL plaintext and
    // purges history — anything the peer pushed that this device never pulled
    // would be destroyed and unrecoverable. Require a fully-pulled anchor.
    let anchored_state = state::load_state();
    if let Some(rh) = git.remote_head()? {
        if anchored_state.last_synced_commit.as_deref() != Some(rh.as_str()) {
            anyhow::bail!(
                "remote has commits this device hasn't pulled — run `claude-xsync pull` first, then rekey"
            );
        }
    }
    if git.diverged()? {
        git.reset_hard_origin()?;
    } else {
        git.pull_ff()?;
    }

    crate::cli::ensure_repo_attributes(&repo)?;

    let home = config::home_dir();
    let mapper = PathMapper::new(&home.to_string_lossy(), &cfg.path_map)?;
    let (locals, _) = collect_locals(&cfg, &claude_dir, &home, &mapper)?;

    // Guard (review v0.1.20): the commit anchor alone does not prove content
    // arrived — a pull that SKIPPED entries (corrupt object, unmapped token)
    // still anchors the commit. Rekey rebuilds the manifest from locals and
    // purges the history that still holds the skipped content, so refuse
    // while any in-sync-set entry is neither anchored nor reproducible here.
    if let Some(old) = read_manifest(&repo, &load_keys(&cfg, &repo)?)? {
        let undelivered: Vec<&String> = old
            .entries
            .iter()
            .filter(|(p, _)| p.as_str() == MCP_PORTABLE || crate::scan::is_synced_rel(p, &cfg))
            .filter(|(p, e)| {
                anchored_state.files.get(*p) != Some(&e.plaintext_hash)
                    && locals.get(*p).map(|l| &l.portable_hash) != Some(&e.plaintext_hash)
            })
            .map(|(p, _)| p)
            .collect();
        if !undelivered.is_empty() && !force {
            anyhow::bail!(
                "{} remote entries were never delivered to this device (a pull skipped them): {}{} — rekey would purge them unrecoverably. Pull until clean (0 skipped) first, or re-run with --force to discard them",
                undelivered.len(),
                undelivered
                    .iter()
                    .take(3)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if undelivered.len() > 3 { ", …" } else { "" }
            );
        }
    }

    // new salt → new keys
    let mut salt = [0u8; 32];
    getrandom::getrandom(&mut salt)
        .map_err(|e| anyhow::anyhow!("cannot generate random salt: {e}"))?;
    crate::fsx::atomic_write(&repo.join("salt"), hex::encode(salt).as_bytes())?;
    let keys = crypto::derive(&new_pass, &salt);

    // wipe old objects, re-seal everything from local plaintext
    let objects_dir = repo.join("objects");
    if objects_dir.is_dir() {
        std::fs::remove_dir_all(&objects_dir)?;
    }

    let mut entries = BTreeMap::new();
    let mut new_state = State::default();
    for (portable, lf) in &locals {
        let object = object_name(&keys.hmac_key, portable);
        let sealed = crypto::seal(&lf.payload, &keys.recipient);
        let chunks = split_chunks(&sealed);
        for (chunk, rel) in chunks.iter().zip(chunk_paths(&object, chunks.len() as u32)) {
            crate::fsx::atomic_write(&repo.join(rel), chunk)?;
        }
        entries.insert(
            portable.clone(),
            Entry {
                object,
                chunks: chunks.len() as u32,
                plaintext_hash: lf.portable_hash.clone(),
                size: lf.payload.len() as u64,
                mode: lf.mode,
                mtime: lf.mtime,
            },
        );
        new_state
            .files
            .insert(portable.clone(), lf.portable_hash.clone());
    }
    write_manifest(
        &repo,
        &keys,
        &Manifest {
            version: 1,
            last_push_device: cfg.device.clone(),
            last_push_ts: unix_now(),
            // registry resets to us alone: the old manifest needs the old key,
            // and every peer re-pushes after a rekey anyway
            devices: std::iter::once(cfg.device.clone()).collect(),
            entries,
        },
    )?;

    // mandatory squash: purge old-key history
    git.squash_to_single_commit("xsync rekey")?;

    new_state.last_synced_commit = Some(git.head()?);
    state::save_state(&new_state)?;

    let env_name = cfg.passphrase_env.as_deref().unwrap_or("XSYNC_PASSPHRASE");
    println!(
        "rekeyed {} files and squashed history — update {env_name} to the new passphrase on EVERY device, then run pull there",
        new_state.files.len()
    );
    Ok(0)
}
