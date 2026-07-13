use crate::cli::{
    collect_locals, guard_running, load_keys, read_manifest, repo_dir, unix_now, write_manifest,
    Summary,
};
use crate::config;
use crate::crypto::object_name;
use crate::gitx::Git;
use crate::manifest::{chunk_paths, split_chunks, Entry, Manifest};
use crate::mapper::PathMapper;
use crate::state;

pub struct PushOpts {
    pub dry_run: bool,
    pub force: bool,
}

pub fn run_push(opts: PushOpts) -> anyhow::Result<i32> {
    let cfg = config::load_config()?;
    let claude_dir = config::claude_dir();
    guard_running(&claude_dir, opts.force)?;

    let repo = repo_dir();
    let git = Git::clone_or_open(&cfg.remote, &repo)?;
    git.fetch()?;
    if git.diverged()? {
        // repo/ is derived data — realign to the remote
        git.reset_hard_origin()?;
    } else {
        git.pull_ff()?;
    }

    let keys = load_keys(&cfg, &repo)?;
    let manifest = read_manifest(&repo, &keys)?;
    let mut st = state::load_state();

    // Guard A: remote moved past our anchor and the last push wasn't ours
    if let (Some(rh), Some(m)) = (git.remote_head()?, manifest.as_ref()) {
        let anchored = st.last_synced_commit.as_deref() == Some(rh.as_str());
        if !anchored && m.last_push_device != cfg.device && !opts.force {
            anyhow::bail!(
                "remote has newer data pushed by {:?} — run `claude-xsync pull` first (or --force)",
                m.last_push_device
            );
        }
    }

    let home = config::home_dir();
    let mapper = PathMapper::new(&home.to_string_lossy(), &cfg.path_map)?;
    let (locals, unknown) = collect_locals(&cfg, &claude_dir, &home, &mapper)?;
    for u in &unknown {
        println!("⚠ unknown top-level entry not synced: {u} (add it to extra_paths in config.toml to sync)");
    }

    let mut summary = Summary::new();
    let mut entries = manifest
        .as_ref()
        .map(|m| m.entries.clone())
        .unwrap_or_default();
    let mut pending_state: Vec<(String, String)> = Vec::new();

    for (portable, lf) in &locals {
        let unchanged = st.files.get(portable) == Some(&lf.portable_hash)
            && entries
                .get(portable)
                .map(|e| e.plaintext_hash == lf.portable_hash)
                .unwrap_or(false);
        if unchanged {
            continue; // age is non-deterministic; the portable-payload hash is the identity
        }
        if let Some(reason) = &lf.verbatim_reason {
            summary.verbatim += 1;
            summary.verbatim_reasons.push(reason.clone());
            println!("⚠ {portable}: stored verbatim ({reason})");
        }
        if opts.dry_run {
            println!("would push {portable}");
            summary.synced += 1;
            continue;
        }
        let object = object_name(&keys.hmac_key, portable);
        remove_object_files(&repo, &object)?;
        let sealed = crate::crypto::seal(&lf.payload, &keys.recipient);
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
            },
        );
        pending_state.push((portable.clone(), lf.portable_hash.clone()));
        summary.synced += 1;
    }

    // Deletions: previously-synced portables that vanished locally
    let deletions: Vec<String> = st
        .files
        .keys()
        .filter(|p| !locals.contains_key(*p))
        .cloned()
        .collect();
    if !opts.dry_run {
        for portable in &deletions {
            if let Some(entry) = entries.remove(portable) {
                remove_object_files(&repo, &entry.object)?;
            }
        }
    }

    if opts.dry_run {
        for portable in &deletions {
            println!("would delete {portable} from remote");
        }
        summary.print();
        return Ok(summary.exit_code());
    }

    if summary.synced > 0 || !deletions.is_empty() {
        write_manifest(
            &repo,
            &keys,
            &Manifest {
                version: 1,
                last_push_device: cfg.device.clone(),
                last_push_ts: unix_now(),
                entries,
            },
        )?;
        git.commit_all(&format!("xsync push from {}", cfg.device))?;
        git.push()?;

        st.last_synced_commit = Some(git.head()?);
        for portable in &deletions {
            st.files.remove(portable);
        }
        state::save_state(&st)?;
        for (portable, hash) in &pending_state {
            state::upsert_and_save(&mut st, portable, hash)?;
        }
    }

    summary.print();
    Ok(summary.exit_code())
}

/// Drop every `objects/<object>.age*` file (stale chunk cleanup before rewrite).
fn remove_object_files(repo: &std::path::Path, object: &str) -> anyhow::Result<()> {
    let dir = repo.join("objects");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let prefix = format!("{object}.age");
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == prefix || name.starts_with(&format!("{prefix}.")) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
