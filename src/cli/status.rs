use crate::cli::{collect_locals, load_keys, read_manifest, repo_dir};
use crate::config;
use crate::gitx::Git;
use crate::mapper::PathMapper;
use crate::state;

pub fn run_status(offline: bool) -> anyhow::Result<i32> {
    let cfg = config::load_config()?;
    let repo = repo_dir();
    let git = Git::clone_or_open(&cfg.remote, &repo)?;
    if !offline {
        git.fetch()?;
        if git.diverged()? {
            println!("note: remote history was rewritten — run `claude-xsync pull` to realign");
        } else {
            git.pull_ff()?;
        }
    }
    let keys = load_keys(&cfg, &repo)?;
    let Some(manifest) = read_manifest(&repo, &keys)? else {
        println!("remote is empty — nothing synced yet");
        return Ok(0);
    };

    let claude_dir = config::claude_dir();
    let home = config::home_dir();
    let mapper = PathMapper::new(&home.to_string_lossy(), &cfg.path_map)?;
    let (locals, _) = collect_locals(&cfg, &claude_dir, &home, &mapper)?;
    let st = state::load_state();

    let to_push: Vec<&String> = locals
        .iter()
        .filter(|(p, lf)| st.files.get(*p) != Some(&lf.portable_hash))
        .map(|(p, _)| p)
        .collect();
    let to_pull: Vec<&String> = manifest
        .entries
        .iter()
        .filter(|(p, e)| st.files.get(*p) != Some(&e.plaintext_hash))
        .map(|(p, _)| p)
        .collect();
    let conflicts = to_push.iter().filter(|p| to_pull.contains(p)).count();

    println!(
        "last push: {} at unix {}",
        manifest.last_push_device, manifest.last_push_ts
    );
    println!(
        "to push: {} · to pull: {} · conflicts: {}",
        to_push.len(),
        to_pull.len(),
        conflicts
    );
    Ok(0)
}
