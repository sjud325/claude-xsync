use crate::cli::{
    collect_locals, guard_running, load_keys, portable_to_rel, read_manifest, repo_dir, unix_now,
    Summary, MCP_PORTABLE,
};
use crate::config;
use crate::crypto::Keys;
use crate::gitx::Git;
use crate::manifest::{chunk_paths, Entry, EntryMode};
use crate::mapper::PathMapper;
use crate::scan::sha256_bytes;
use crate::special::{history_merge::union_jsonl, mcp::merge_mcp};
use crate::state;
use crate::transform::file::{normalize_file, resolve_file_pull, TransformOutcome};
use std::collections::BTreeSet;
use std::path::Path;

pub struct PullOpts {
    pub dry_run: bool,
    pub force: bool,
}

/// Staged resolutions — nothing under ~/.claude changes until every entry
/// has been staged (spec §7 apply procedure).
enum Planned {
    Write {
        rel: String,
        bytes: Vec<u8>,
        portable: String,
        hash: String,
        conflict_local: Option<Vec<u8>>,
    },
    McpMerge {
        subtree: String,
        hash: String,
    },
    Delete {
        rel: Option<String>,
        portable: String,
    },
    StateOnly {
        portable: String,
        hash: String,
    },
}

pub fn run_pull(opts: PullOpts) -> anyhow::Result<i32> {
    let cfg = config::load_config()?;
    let claude_dir = config::claude_dir();
    guard_running(&claude_dir, opts.force)?;

    let repo = repo_dir();
    let git = Git::clone_or_open(&cfg.remote, &repo)?;
    git.fetch()?;
    let mut re_anchor = false;
    if git.diverged()? {
        println!("remote history was rewritten (gc --squash / rekey) — resetting local mirror");
        git.reset_hard_origin()?;
        re_anchor = true;
    } else {
        git.pull_ff()?;
    }

    // keys AFTER fetch/reset — a rekey may have replaced the salt
    let keys = load_keys(&cfg, &repo)?;
    let mut summary = Summary::new();
    let Some(manifest) = read_manifest(&repo, &keys)? else {
        println!("remote is empty — nothing to pull");
        summary.print();
        return Ok(0);
    };

    let home = config::home_dir();
    let mapper = PathMapper::new(&home.to_string_lossy(), &cfg.path_map)?;
    let (locals, _unknown) = collect_locals(&cfg, &claude_dir, &home, &mapper)?;
    let mut st = state::load_state();

    if re_anchor {
        // History was force-rewritten. Drop every anchor the fresh manifest
        // does not corroborate: if this device's push lost a race against the
        // rewrite, its content then classifies as both-modified (conflict
        // copy) instead of "remote-only changed" (silent revert). Review I3.
        st.files.retain(|portable, hash| {
            manifest
                .entries
                .get(portable)
                .map(|e| &e.plaintext_hash == hash)
                .unwrap_or(false)
        });
        // Files already matching the rewritten remote get anchored directly.
        for (portable, entry) in &manifest.entries {
            if locals.get(portable).map(|l| &l.portable_hash) == Some(&entry.plaintext_hash) {
                st.files
                    .insert(portable.clone(), entry.plaintext_hash.clone());
            }
        }
        state::save_state(&st)?;
    }

    // ---- classify (spec §7 table) + stage ----
    let mut planned: Vec<Planned> = Vec::new();
    let all_keys: BTreeSet<String> = manifest
        .entries
        .keys()
        .chain(st.files.keys())
        .chain(locals.keys())
        .cloned()
        .collect();

    for portable in all_keys {
        let entry = manifest.entries.get(&portable);
        let local = locals.get(&portable);
        let state_h = st.files.get(&portable).cloned();
        match (local, &state_h, entry) {
            // local-only new — preserve; joins the remote on the next push
            (Some(_), None, None) | (None, None, None) => {}
            // remote deleted — move local to backup, forget state
            (_, Some(_), None) => {
                planned.push(Planned::Delete {
                    rel: local.and_then(|l| l.rel.clone()),
                    portable: portable.clone(),
                });
            }
            (local, state_h, Some(e)) => {
                let local_h = local.map(|l| l.portable_hash.clone());
                let local_changed = local_h.as_deref() != state_h.as_deref();
                let remote_changed = Some(e.plaintext_hash.as_str()) != state_h.as_deref();
                if !remote_changed {
                    continue; // in-sync or local-only changed — preserve (push target)
                }
                if local_h.as_deref() == Some(e.plaintext_hash.as_str()) {
                    planned.push(Planned::StateOnly {
                        portable: portable.clone(),
                        hash: e.plaintext_hash.clone(),
                    });
                    continue;
                }
                let bytes = match stage_entry(&repo, &keys, &portable, e, &mapper) {
                    Ok(b) => b,
                    Err(err) => {
                        println!("✗ skipping {portable}: {err:#}");
                        summary.skipped += 1;
                        continue;
                    }
                };
                if portable == MCP_PORTABLE {
                    match String::from_utf8(bytes) {
                        Ok(subtree) => planned.push(Planned::McpMerge {
                            subtree,
                            hash: e.plaintext_hash.clone(),
                        }),
                        Err(_) => {
                            println!("✗ skipping {portable}: mcp subtree is not utf-8");
                            summary.skipped += 1;
                        }
                    }
                    continue;
                }
                let rel = match portable_to_rel(&portable, &mapper) {
                    Ok(r) => r,
                    Err(err) => {
                        println!("✗ skipping {portable}: {err}");
                        summary.skipped += 1;
                        continue;
                    }
                };
                let local_exists_changed = local.is_some() && local_changed;
                if !local_exists_changed {
                    planned.push(Planned::Write {
                        rel,
                        bytes,
                        portable: portable.clone(),
                        hash: e.plaintext_hash.clone(),
                        conflict_local: None,
                    });
                } else if rel == "history.jsonl" {
                    // append-only special case: line-set union, no conflict copy
                    let merged = union_jsonl(&local.unwrap().raw, &bytes);
                    planned.push(Planned::Write {
                        rel,
                        bytes: merged,
                        portable: portable.clone(),
                        hash: e.plaintext_hash.clone(),
                        conflict_local: None,
                    });
                } else {
                    summary.conflicts += 1;
                    planned.push(Planned::Write {
                        rel,
                        bytes,
                        portable: portable.clone(),
                        hash: e.plaintext_hash.clone(),
                        conflict_local: Some(local.unwrap().raw.clone()),
                    });
                }
            }
        }
    }

    if opts.dry_run {
        for p in &planned {
            match p {
                Planned::Write {
                    rel,
                    conflict_local,
                    ..
                } => println!(
                    "would apply {rel}{}",
                    if conflict_local.is_some() {
                        " (conflict — local copy kept)"
                    } else {
                        ""
                    }
                ),
                Planned::McpMerge { .. } => println!("would merge mcpServers into ~/.claude.json"),
                Planned::Delete { rel, portable } => {
                    println!(
                        "would remove {} (deleted on remote)",
                        rel.as_deref().unwrap_or(portable)
                    )
                }
                Planned::StateOnly { portable, .. } => println!("already in sync: {portable}"),
            }
        }
        summary.print();
        return Ok(summary.exit_code());
    }

    // ---- apply (backup → atomic writes → state per file) ----
    let stamp = unix_now().to_string();
    let backup_rels: Vec<String> = planned
        .iter()
        .filter_map(|p| match p {
            Planned::Write { rel, .. } => claude_dir.join(rel).exists().then(|| rel.clone()),
            Planned::Delete { rel: Some(rel), .. } => Some(rel.clone()),
            _ => None,
        })
        .collect();
    if !backup_rels.is_empty() {
        let bdir = crate::fsx::backup_files(&claude_dir, &backup_rels, &stamp)?;
        println!("backup of replaced files: {}", bdir.display());
    }

    let claude_json_path = home.join(".claude.json");
    for p in planned {
        match p {
            Planned::Write {
                rel,
                bytes,
                portable,
                hash,
                conflict_local,
            } => {
                if let Some(local_raw) = conflict_local {
                    let cpath = claude_dir.join(format!("{rel}.xsync-conflict.{stamp}"));
                    crate::fsx::atomic_write(&cpath, &local_raw)?;
                    println!(
                        "⚡ conflict on {rel}: your version kept at {}",
                        cpath.display()
                    );
                }
                crate::fsx::atomic_write(&claude_dir.join(&rel), &bytes)?;
                state::upsert_and_save(&mut st, &portable, &hash)?;
                summary.synced += 1;
            }
            Planned::McpMerge { subtree, hash } => {
                let current =
                    std::fs::read_to_string(&claude_json_path).unwrap_or_else(|_| "{}".into());
                let merged = merge_mcp(&current, &subtree)?;
                crate::fsx::atomic_write(&claude_json_path, merged.as_bytes())?;
                state::upsert_and_save(&mut st, MCP_PORTABLE, &hash)?;
                summary.synced += 1;
            }
            Planned::Delete { rel, portable } => {
                if let Some(rel) = rel {
                    let path = claude_dir.join(&rel);
                    if path.is_file() {
                        std::fs::remove_file(&path)?; // already copied into the backup dir
                        println!("removed {rel} (deleted on remote; backup kept)");
                    }
                }
                st.files.remove(&portable);
                state::save_state(&st)?;
            }
            Planned::StateOnly { portable, hash } => {
                state::upsert_and_save(&mut st, &portable, &hash)?;
            }
        }
    }

    st.last_synced_commit = Some(git.head()?);
    state::save_state(&st)?;
    summary.print();
    Ok(summary.exit_code())
}

/// Decrypt + integrity-check an object; Transformed entries are resolved to
/// this device's paths and reverse-verified (`normalize(resolved) == payload`).
fn stage_entry(
    repo: &Path,
    keys: &Keys,
    portable: &str,
    e: &Entry,
    mapper: &PathMapper,
) -> anyhow::Result<Vec<u8>> {
    let mut sealed = Vec::new();
    for rel in chunk_paths(&e.object, e.chunks) {
        sealed.extend(
            std::fs::read(repo.join(&rel))
                .map_err(|err| anyhow::anyhow!("missing object chunk {rel}: {err}"))?,
        );
    }
    let payload = crate::crypto::open(&sealed, &keys.identity)?;
    if sha256_bytes(&payload) != e.plaintext_hash {
        anyhow::bail!("object integrity check failed");
    }
    match e.mode {
        EntryMode::Verbatim => Ok(payload),
        EntryMode::Transformed => {
            let resolved = resolve_file_pull(portable, &payload, mapper)
                .map_err(|err| anyhow::anyhow!("{err}"))?;
            match normalize_file(portable, &resolved, mapper) {
                TransformOutcome::Transformed { data, .. } if data == payload => Ok(resolved),
                TransformOutcome::Transformed { data: d2, .. } => {
                    // One-step stability (spec §12.2, decision C′): quoted
                    // peer-home text re-tokenizes under THIS device's mapper.
                    // That is absorption, not corruption — provided the
                    // re-tokenized form is its own round-trip fixpoint
                    // (normalize(resolve(d2)) == d2). Byte-comparing against
                    // `resolved` would be too strict: pull emits the slash
                    // form, so backslash/case variants of the quote would
                    // never absorb on Windows even though they converge.
                    let stable = resolve_file_pull(portable, &d2, mapper)
                        .ok()
                        .map(|r2| match normalize_file(portable, &r2, mapper) {
                            TransformOutcome::Transformed { data, .. } => data == d2,
                            TransformOutcome::Verbatim { .. } => false,
                        })
                        .unwrap_or(false);
                    if stable {
                        println!(
                            "⚠ {portable}: quoted peer-device path text absorbed as live local paths (one-time; see README)"
                        );
                        Ok(resolved)
                    } else {
                        anyhow::bail!(
                            "pull reverse-verify failed (resolved form does not re-normalize)"
                        )
                    }
                }
                _ => anyhow::bail!(
                    "pull reverse-verify failed (resolved form does not re-normalize)"
                ),
            }
        }
    }
}
