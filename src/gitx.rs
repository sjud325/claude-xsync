use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Thin `git` CLI wrapper. Every failure carries git's stderr verbatim.
#[derive(Debug)]
pub struct Git {
    pub repo: PathBuf,
}

impl Git {
    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        run_git(&self.repo, args)
    }

    pub fn clone_or_open(remote: &str, repo: &Path) -> anyhow::Result<Git> {
        if repo.join(".git").is_dir() {
            return Ok(Git { repo: repo.to_path_buf() });
        }
        let parent = repo.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        run_git(parent, &["clone", remote, &repo.to_string_lossy()])?;
        let git = Git { repo: repo.to_path_buf() };
        // Standardize on `main` even when cloning an empty remote.
        if git.run(&["rev-parse", "--abbrev-ref", "HEAD"]).map(|b| b != "main").unwrap_or(true) {
            git.run(&["checkout", "-B", "main"])?;
        }
        Ok(git)
    }

    pub fn fetch(&self) -> anyhow::Result<()> {
        self.run(&["fetch", "origin"]).map(|_| ())
    }

    pub fn head(&self) -> anyhow::Result<String> {
        self.run(&["rev-parse", "HEAD"])
    }

    /// origin/main, or None when the remote has no commits yet.
    pub fn remote_head(&self) -> anyhow::Result<Option<String>> {
        Ok(self.run(&["rev-parse", "origin/main"]).ok())
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> anyhow::Result<bool> {
        let status = Command::new("git")
            .args(["merge-base", "--is-ancestor", a, b])
            .current_dir(&self.repo)
            .output()
            .context("failed to spawn git")?;
        Ok(status.status.success())
    }

    /// Neither HEAD nor origin/main is an ancestor of the other (force-push signal).
    pub fn diverged(&self) -> anyhow::Result<bool> {
        let Ok(head) = self.head() else { return Ok(false) };
        let Some(remote) = self.remote_head()? else { return Ok(false) };
        if head == remote {
            return Ok(false);
        }
        Ok(!self.is_ancestor(&head, &remote)? && !self.is_ancestor(&remote, &head)?)
    }

    pub fn reset_hard_origin(&self) -> anyhow::Result<()> {
        self.fetch()?;
        self.run(&["reset", "--hard", "origin/main"]).map(|_| ())
    }

    /// `git add -A` + commit (no-op when the tree is clean); returns HEAD hash.
    pub fn commit_all(&self, msg: &str) -> anyhow::Result<String> {
        self.run(&["add", "-A"])?;
        let dirty = !self.run(&["status", "--porcelain"])?.is_empty();
        if dirty {
            self.run(&[
                "-c", "user.name=claude-xsync",
                "-c", "user.email=xsync@localhost",
                "commit", "-m", msg,
            ])?;
        }
        self.head()
    }

    pub fn push(&self) -> anyhow::Result<()> {
        self.run(&["push", "origin", "main"]).map(|_| ())
    }

    pub fn push_force(&self) -> anyhow::Result<()> {
        self.run(&["push", "--force", "origin", "main"]).map(|_| ())
    }

    /// fetch + merge --ff-only (falls back to reset for an unborn local branch).
    pub fn pull_ff(&self) -> anyhow::Result<()> {
        self.fetch()?;
        if self.remote_head()?.is_none() {
            return Ok(()); // empty remote — nothing to merge
        }
        if self.head().is_err() {
            return self.run(&["reset", "--hard", "origin/main"]).map(|_| ());
        }
        self.run(&["merge", "--ff-only", "origin/main"]).map(|_| ())
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to spawn git — is git installed?")?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed ({}): {}",
            args.first().unwrap_or(&"?"),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
