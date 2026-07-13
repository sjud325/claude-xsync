use clap::{Parser, Subcommand};
use claude_xsync::cli;

#[derive(Parser)]
#[command(name = "claude-xsync", version, about = "Cross-platform ~/.claude sync over an encrypted git repo")]
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
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Init(opts) => cli::init::run_init(opts),
        Cmd::Push { dry_run, force } => cli::push::run_push(cli::push::PushOpts { dry_run, force }),
        Cmd::Pull { .. } => Err(anyhow::anyhow!("pull is not implemented yet")),
        Cmd::Status { .. } => Err(anyhow::anyhow!("status is not implemented yet")),
        Cmd::Gc { .. } => Err(anyhow::anyhow!("gc is not implemented yet")),
        Cmd::Rekey { .. } => Err(anyhow::anyhow!("rekey is not implemented yet")),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
    }
}
