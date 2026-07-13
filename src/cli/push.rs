use crate::cli::{
    guard_running, load_keys, read_manifest, rel_to_portable, repo_dir, unix_now, write_manifest,
    Summary,
};
use crate::config;
use crate::crypto::object_name;
use crate::gitx::Git;
use crate::manifest::{chunk_paths, split_chunks, Entry, EntryMode, Manifest};
use crate::mapper::PathMapper;
use crate::scan::{scan, sha256_bytes};
use crate::special::{mcp::extract_mcp, plugins::plugin_manifest_rels};
use crate::state;
use crate::transform::file::TransformOutcome;
use crate::verify::push_gate;
use std::collections::BTreeMap;

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
    let mut manifest = read_manifest(&repo, &keys)?;
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

    // Work list: scanned files + synthetic files (mcp subtree, plugin manifests)
    let scanres = scan(&claude_dir, &cfg)?;
    for u in &scanres.unknown {
        println!("⚠ unknown top-level entry not synced: {u} (add it to extra_paths in config.toml to sync)");
    }
    let mut items: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (rel, path) in &scanres.files {
        items.insert(rel_to_portable(rel, &mapper), std::fs::read(path)?);
    }
    for rel in plugin_manifest_rels(&claude_dir.join("plugins")) {
        items.insert(rel.clone(), std::fs::read(claude_dir.join(&rel))?);
    }
    if let Ok(claude_json) = std::fs::read_to_string(home.join(".claude.json")) {
        if let Some(subtree) = extract_mcp(&claude_json)? {
            items.insert("_xsync/mcp-servers.json".into(), subtree.into_bytes());
        }
    }

    let mut summary = Summary::new();
    let mut entries = manifest.as_ref().map(|m| m.entries.clone()).unwrap_or_default();
    let mut pending_state: Vec<(String, String)> = Vec::new();

    for (portable, data) in &items {
        let hash = sha256_bytes(data);
        let unchanged = entries.get(portable).map(|e| e.plaintext_hash == hash).unwrap_or(false)
            && st.files.get(portable) == Some(&hash);
        if unchanged {
            continue; // age is non-deterministic; plaintext hash is the identity
        }
        let (payload, mode) = match push_gate(portable, data, &mapper) {
            TransformOutcome::Transformed { data: t, .. } => (t, EntryMode::Transformed),
            TransformOutcome::Verbatim { reason } => {
                summary.verbatim += 1;
                summary.verbatim_reasons.push(reason);
                (data.clone(), EntryMode::Verbatim)
            }
        };
        if opts.dry_run {
            println!("would push {portable}");
            summary.synced += 1;
            continue;
        }
        let object = object_name(&keys.hmac_key, portable);
        remove_object_files(&repo, &object)?;
        let sealed = crate::crypto::seal(&payload, &keys.recipient);
        let chunks = split_chunks(&sealed);
        for (chunk, rel) in chunks.iter().zip(chunk_paths(&object, chunks.len() as u32)) {
            crate::fsx::atomic_write(&repo.join(rel), chunk)?;
        }
        entries.insert(
            portable.clone(),
            Entry {
                object,
                chunks: chunks.len() as u32,
                plaintext_hash: hash.clone(),
                size: data.len() as u64,
                mode,
            },
        );
        pending_state.push((portable.clone(), hash));
        summary.synced += 1;
    }

    // Deletions: previously-synced portables that vanished locally
    let mut deletions = Vec::new();
    for portable in st.files.keys() {
        if !items.contains_key(portable) {
            deletions.push(portable.clone());
        }
    }
    if !opts.dry_run {
        for portable in &deletions {
            if let Some(entry) = entries.remove(portable) {
                remove_object_files(&repo, &entry.object)?;
            }
        }
    }

    if opts.dry_run {
        summary.print();
        return Ok(summary.exit_code());
    }

    if summary.synced > 0 || !deletions.is_empty() {
        let new_manifest = Manifest {
            version: 1,
            last_push_device: cfg.device.clone(),
            last_push_ts: unix_now(),
            entries,
        };
        write_manifest(&repo, &keys, &new_manifest)?;
        git.commit_all(&format!("xsync push from {}", cfg.device))?;
        git.push()?;
        manifest = Some(new_manifest);
        let _ = manifest; // manifest now reflects the pushed state

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
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(()) };
    let prefix = format!("{object}.age");
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == prefix || name.starts_with(&format!("{prefix}.")) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
