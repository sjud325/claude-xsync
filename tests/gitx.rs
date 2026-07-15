use claude_xsync::gitx::Git;
use std::fs;
use std::process::Command;

fn make_bare(dir: &std::path::Path) -> String {
    fs::create_dir_all(dir).unwrap();
    let out = Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir.to_string_lossy().to_string()
}

#[test]
fn clone_commit_push_second_clone_sees_it() {
    let td = tempfile::tempdir().unwrap();
    let url = make_bare(&td.path().join("remote.git"));

    let a = Git::clone_or_open(&url, &td.path().join("a")).unwrap();
    fs::write(a.repo.join("hello.txt"), b"v1").unwrap();
    let h = a.commit_all("c1").unwrap();
    assert!(!h.is_empty());
    a.push().unwrap();

    let b = Git::clone_or_open(&url, &td.path().join("b")).unwrap();
    assert_eq!(fs::read(b.repo.join("hello.txt")).unwrap(), b"v1");
    assert_eq!(b.head().unwrap(), h);
}

#[test]
fn force_push_divergence_detected_and_recovered() {
    let td = tempfile::tempdir().unwrap();
    let url = make_bare(&td.path().join("remote.git"));

    let a = Git::clone_or_open(&url, &td.path().join("a")).unwrap();
    fs::write(a.repo.join("f.txt"), b"one").unwrap();
    a.commit_all("c1").unwrap();
    a.push().unwrap();

    let b = Git::clone_or_open(&url, &td.path().join("b")).unwrap();
    assert!(!b.diverged().unwrap());

    // rewrite history on a (amend) and force-push
    fs::write(a.repo.join("f.txt"), b"one-rewritten").unwrap();
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-a",
            "--amend",
            "-m",
            "c1'",
        ])
        .current_dir(&a.repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    a.push_force().unwrap();

    b.fetch().unwrap();
    assert!(b.diverged().unwrap());
    b.reset_hard_origin().unwrap();
    assert!(!b.diverged().unwrap());
    assert_eq!(fs::read(b.repo.join("f.txt")).unwrap(), b"one-rewritten");
    assert_eq!(b.head().unwrap(), b.remote_head().unwrap().unwrap());
}

#[test]
fn pull_ff_survives_untracked_gitattributes_collision() {
    // Real-machine failure (2026-07-15): init plants .gitattributes without
    // committing it; once the peer's push added a tracked copy, the merge
    // refused to overwrite the untracked file and pull aborted.
    let td = tempfile::tempdir().unwrap();
    let url = make_bare(&td.path().join("remote.git"));

    let a = Git::clone_or_open(&url, &td.path().join("a")).unwrap();
    fs::write(a.repo.join("f.txt"), b"one").unwrap();
    a.commit_all("c1").unwrap();
    a.push().unwrap();

    // b clones while the remote has no .gitattributes, then plants an
    // untracked copy (exactly what init/ensure_repo_attributes does)
    let b = Git::clone_or_open(&url, &td.path().join("b")).unwrap();
    const ATTRS: &[u8] = b"# encrypted blobs only -- never translate newlines\n* -text\n";
    fs::write(b.repo.join(".gitattributes"), ATTRS).unwrap();

    // the peer's next push commits the identical file
    fs::write(a.repo.join(".gitattributes"), ATTRS).unwrap();
    a.commit_all("attrs").unwrap();
    a.push().unwrap();

    b.fetch().unwrap();
    assert!(!b.diverged().unwrap());
    b.pull_ff().unwrap(); // must not abort on the untracked collision
    assert_eq!(fs::read(b.repo.join(".gitattributes")).unwrap(), ATTRS);
    assert_eq!(b.head().unwrap(), b.remote_head().unwrap().unwrap());
}

#[test]
fn error_includes_git_stderr() {
    let td = tempfile::tempdir().unwrap();
    let err = Git::clone_or_open("/nonexistent/definitely-missing.git", &td.path().join("x"))
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("git"), "error should carry git context: {msg}");
}
