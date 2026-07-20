use clap::{Parser, Subcommand};
use claude_xsync::cli;

#[derive(Parser)]
#[command(
    name = "claude-xsync",
    version,
    about = "Cross-platform ~/.claude sync over an encrypted git repo"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set up this device (clone remote, create/read salt, write config)
    Init(cli::init::InitOpts),
    /// Encrypt local changes and push them to the remote
    Push {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Fetch remote changes and apply them locally (5-way classification)
    Pull {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Show to-push / to-pull / conflict counts
    Status {
        #[arg(long)]
        offline: bool,
    },
    /// Repo maintenance
    Gc {
        /// Rewrite remote history into a single commit
        #[arg(long)]
        squash: bool,
    },
    /// Re-encrypt everything under a new passphrase (implies squash)
    Rekey {
        /// Env var NAME that holds the NEW passphrase
        #[arg(long, default_value = "XSYNC_NEW_PASSPHRASE")]
        passphrase_env: String,
        /// Proceed even when remote entries were never delivered here
        /// (they are purged unrecoverably)
        #[arg(long)]
        force: bool,
    },
    /// Insert the multi-device resume note into ~/.claude/CLAUDE.md
    ClaudeMd,
    /// Add synced sessions to the Claude desktop app's session list
    AppIndex {
        #[arg(long)]
        dry_run: bool,
        /// Also index command-only sessions (no conversation content)
        #[arg(long)]
        all: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Init(opts) => cli::init::run_init(opts),
        Cmd::Push { dry_run, force } => cli::push::run_push(cli::push::PushOpts { dry_run, force }),
        Cmd::Pull { dry_run, force } => cli::pull::run_pull(cli::pull::PullOpts { dry_run, force }),
        Cmd::Status { offline } => cli::status::run_status(offline),
        Cmd::Gc { squash } => cli::gc::run_gc(squash),
        Cmd::Rekey {
            passphrase_env,
            force,
        } => cli::rekey::run_rekey(passphrase_env, force),
        Cmd::ClaudeMd => cli::claude_md::run_claude_md(),
        Cmd::AppIndex { dry_run, all } => cli::app_index::run_app_index(dry_run, all),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
    }
}
