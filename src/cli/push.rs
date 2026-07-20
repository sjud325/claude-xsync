use crate::cli::{
    collect_locals, guard_running, load_keys, read_manifest, repo_dir, unix_now, write_manifest,
    Summary, MCP_PORTABLE,
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

    crate::cli::ensure_repo_attributes(&repo)?;
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

    // age is non-deterministic; the portable-payload hash is the identity
    let unchanged: std::collections::BTreeSet<String> = locals
        .iter()
        .filter(|(portable, lf)| {
            st.files.get(*portable) == Some(&lf.portable_hash)
                && entries
                    .get(*portable)
                    .map(|e| e.plaintext_hash == lf.portable_hash)
                    .unwrap_or(false)
        })
        .map(|(portable, _)| portable.clone())
        .collect();
    let to_seal = locals.len() - unchanged.len();
    if to_seal > 0 && !opts.dry_run {
        println!("sealing {to_seal} changed files…");
    }
    let mut sealed_count = 0usize;
    let mut mtime_backfills = 0usize;

    for (portable, lf) in &locals {
        if unchanged.contains(portable) {
            // pre-0.1.5 manifests carry no mtimes — backfill from this
            // device's originals so pulls can restore --resume ordering
            if let (Some(m), Some(e)) = (lf.mtime, entries.get_mut(portable)) {
                if e.mtime.is_none() {
                    e.mtime = Some(m);
                    mtime_backfills += 1;
                }
            }
            continue;
        }
        // Never overwrite a manifest entry that moved past our anchor — the
        // remote version is newer than what this device last synced (e.g. a
        // pull skipped it). Overwriting would silently regress the peer's
        // data; resolving is pull's job (conflict copies). Review C1.
        if let Some(e) = entries.get(portable) {
            if e.plaintext_hash != lf.portable_hash
                && st.files.get(portable) != Some(&e.plaintext_hash)
            {
                println!(
                    "✗ {portable}: changed on the remote since this device last synced it — run `claude-xsync pull` first"
                );
                summary.skipped += 1;
                continue;
            }
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
        // drop the previous entry's chunk files (covers shrinking chunk
        // counts); scanning the whole objects dir per file was O(N²)
        if let Some(old) = entries.get(portable) {
            remove_entry_objects(&repo, old)?;
        }
        let sealed = crate::crypto::seal(&lf.payload, &keys.recipient);
        let chunks = split_chunks(&sealed);
        for (chunk, rel) in chunks.iter().zip(chunk_paths(&object, chunks.len() as u32)) {
            crate::fsx::atomic_write(&repo.join(rel), chunk)?;
        }
        sealed_count += 1;
        if sealed_count.is_multiple_of(200) {
            println!("· {sealed_count}/{to_seal} sealed");
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
        pending_state.push((portable.clone(), lf.portable_hash.clone()));
        summary.synced += 1;
    }

    // Deletions: previously-synced portables that vanished locally.
    // Same staleness guard as above: if the remote changed the file after our
    // anchor, deleting it here would destroy the peer's newer data (review C1).
    let mut deletions: Vec<String> = Vec::new();
    let mut anchor_forgets: Vec<String> = Vec::new();
    for (portable, anchored_hash) in &st.files {
        if locals.contains_key(portable) {
            continue;
        }
        // "Vanished from locals" can also mean "no longer in this device's
        // sync set" (opt-out, upgrade). Structurally never-synced paths fall
        // through to deletion — every device agrees they don't belong on the
        // remote. A config-dependent miss must only drop OUR anchor: the
        // entry may be a peer's opted-in data (review v0.1.17 finding 1).
        if portable.as_str() != MCP_PORTABLE
            && !crate::scan::is_never_synced_rel(portable)
            && !crate::scan::is_synced_rel(portable, &cfg)
        {
            anchor_forgets.push(portable.clone());
            continue;
        }
        match entries.get(portable) {
            Some(e) if e.plaintext_hash != *anchored_hash => {
                println!(
                    "✗ {portable}: deleted locally but changed on the remote — run `claude-xsync pull` first"
                );
                summary.skipped += 1;
            }
            _ => deletions.push(portable.clone()),
        }
    }
    for portable in &anchor_forgets {
        if opts.dry_run {
            println!("would forget sync state for {portable} (not in this device's sync set)");
        } else {
            st.files.remove(portable);
            println!("forgot sync state for {portable} (not in this device's sync set)");
        }
    }
    if !opts.dry_run && !anchor_forgets.is_empty() {
        state::save_state(&st)?;
    }
    if !opts.dry_run {
        for portable in &deletions {
            if let Some(entry) = entries.remove(portable) {
                remove_entry_objects(&repo, &entry)?;
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

    if mtime_backfills > 0 {
        println!("backfilled timestamps for {mtime_backfills} entries");
    }
    if summary.synced > 0 || !deletions.is_empty() || mtime_backfills > 0 {
        // device registry: carry forward, backfill pre-0.1.15 manifests from
        // last_push_device, and add ourselves
        let mut devices = manifest
            .as_ref()
            .map(|m| m.devices.clone())
            .unwrap_or_default();
        if let Some(m) = manifest.as_ref() {
            devices.insert(m.last_push_device.clone());
        }
        devices.insert(cfg.device.clone());
        write_manifest(
            &repo,
            &keys,
            &Manifest {
                version: 1,
                last_push_device: cfg.device.clone(),
                last_push_ts: unix_now(),
                devices,
                entries,
            },
        )?;
        println!("uploading to remote…");
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

/// Drop exactly the chunk files a manifest entry owns — O(chunks), not a
/// directory scan.
fn remove_entry_objects(repo: &std::path::Path, entry: &Entry) -> anyhow::Result<()> {
    for rel in chunk_paths(&entry.object, entry.chunks) {
        let p = repo.join(&rel);
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}
