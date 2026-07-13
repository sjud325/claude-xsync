use crate::cli::repo_dir;
use crate::config;
use crate::gitx::Git;
use crate::state;

pub fn run_gc(squash: bool) -> anyhow::Result<i32> {
    if !squash {
        println!("nothing to do — v1 implements `gc --squash` only");
        return Ok(0);
    }
    let cfg = config::load_config()?;
    let repo = repo_dir();
    let git = Git::clone_or_open(&cfg.remote, &repo)?;
    git.fetch()?;
    if git.diverged()? {
        git.reset_hard_origin()?;
    } else {
        git.pull_ff()?;
    }
    git.squash_to_single_commit("xsync gc --squash")?;

    let mut st = state::load_state();
    st.last_synced_commit = Some(git.head()?);
    state::save_state(&st)?;
    println!(
        "history squashed to a single commit — the other device will auto-recover on its next pull"
    );
    Ok(0)
}
