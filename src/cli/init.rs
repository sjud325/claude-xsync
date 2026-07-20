use crate::cli::{load_keys, read_manifest, repo_dir};
use crate::config::{self, Config};
use crate::gitx::Git;

#[derive(clap::Args, Debug)]
pub struct InitOpts {
    /// Private git remote URL (create the repo on GitHub yourself first)
    #[arg(long)]
    pub remote: String,
    /// Name of this device (e.g. "mac", "win")
    #[arg(long)]
    pub device: String,
    /// Env var NAME that holds the passphrase (never the passphrase itself)
    #[arg(long, default_value = "XSYNC_PASSPHRASE")]
    pub passphrase_env: String,
    /// Skip adding the multi-device note to ~/.claude/CLAUDE.md
    #[arg(long)]
    pub no_claude_md: bool,
}

pub fn run_init(opts: InitOpts) -> anyhow::Result<i32> {
    let repo = repo_dir();
    let git = Git::clone_or_open(&opts.remote, &repo)?;
    git.pull_ff()?;

    crate::cli::ensure_repo_attributes(&repo)?;

    // salt: create on first device, reuse on the second
    let salt_path = repo.join("salt");
    if !salt_path.exists() {
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt)
            .map_err(|e| anyhow::anyhow!("cannot generate random salt: {e}"))?;
        crate::fsx::atomic_write(&salt_path, hex::encode(salt).as_bytes())?;
        git.commit_all("chore: xsync salt")?;
        git.push()?;
        println!("generated new salt and pushed it to the remote");
    }

    let cfg = Config {
        remote: opts.remote.clone(),
        device: opts.device.clone(),
        passphrase_env: Some(opts.passphrase_env.clone()),
        ..Config::default()
    };

    // key check: if the remote already has a manifest, decrypting it proves
    // the passphrase (wrong passphrase = abort, nothing written)
    let keys = load_keys(&cfg, &repo)?;
    let manifest = read_manifest(&repo, &keys)?;
    let manifest_exists = manifest.is_some();
    if let Some(m) = &manifest {
        println!("existing remote manifest decrypted — passphrase verified");
        // Warning only, never an error: re-init of the SAME machine is a
        // documented recovery flow and legitimately reuses its name.
        let mut known = m.devices.clone();
        known.insert(m.last_push_device.clone());
        if known.contains(&opts.device) {
            println!(
                "⚠ device name {:?} is already used in this sync group — a second machine \
                 under the same name silently disables the pull-before-push guard; \
                 pick a unique name unless this is a re-init of that same machine",
                opts.device
            );
        }
    }

    config::save_config(&cfg)?;

    // First device only: joining devices receive the note via pull instead,
    // which avoids a spurious CLAUDE.md conflict on their first pull.
    if !opts.no_claude_md
        && !manifest_exists
        && crate::cli::claude_md::ensure_note(&config::claude_dir())?
            != crate::cli::claude_md::NoteAction::Unchanged
    {
        println!("added a multi-device note to ~/.claude/CLAUDE.md (managed block — delete it or use --no-claude-md to opt out)");
    }
    println!(
        "initialized device {:?} → {} \nnote: losing the passphrase makes the REMOTE unrecoverable (plaintext stays on your devices; re-init to recover)",
        opts.device, opts.remote
    );
    Ok(0)
}
