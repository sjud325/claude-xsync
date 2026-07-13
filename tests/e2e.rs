//! End-to-end harness: two fake devices sharing one bare git remote.
//! Every path the binary touches is redirected via XSYNC_* env overrides —
//! the real ~/.claude is never read or written.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

pub struct FakeDevice {
    pub home: TempDir,
    pub name: &'static str,
}

impl FakeDevice {
    fn new(name: &'static str) -> FakeDevice {
        FakeDevice {
            home: TempDir::new().unwrap(),
            name,
        }
    }
    pub fn home_str(&self) -> String {
        self.home.path().to_string_lossy().to_string()
    }
    pub fn claude(&self) -> PathBuf {
        self.home.path().join(".claude")
    }
    pub fn xsync(&self) -> PathBuf {
        self.home.path().join(".claude-xsync")
    }
    /// Claude Code's directory-key encoding of this device's home.
    pub fn enc_home(&self) -> String {
        self.home_str()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }
}

pub struct TestEnv {
    pub bare: TempDir,
    pub dev_a: FakeDevice,
    pub dev_b: FakeDevice,
}

impl TestEnv {
    #[allow(clippy::new_without_default)] // test harness, Default is meaningless
    pub fn new() -> TestEnv {
        let bare = TempDir::new().unwrap();
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(bare.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let dev_a = FakeDevice::new("mac");
        let dev_b = FakeDevice::new("win");

        // dev_a: one session with a cwd line + settings.json
        let proj = dev_a
            .claude()
            .join(format!("projects/{}-ws-app", dev_a.enc_home()));
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("s.jsonl"),
            format!(
                "{{\"cwd\":\"{}/ws/app\",\"type\":\"user\"}}\n",
                dev_a.home_str()
            ),
        )
        .unwrap();
        fs::write(
            dev_a.claude().join("settings.json"),
            b"{\"model\":\"opus\"}",
        )
        .unwrap();
        fs::write(
            dev_a.claude().join("history.jsonl"),
            b"{\"display\":\"cmd-a\"}\n",
        )
        .unwrap();

        // dev_b: different settings
        fs::create_dir_all(dev_b.claude()).unwrap();
        fs::write(
            dev_b.claude().join("settings.json"),
            b"{\"model\":\"sonnet\"}",
        )
        .unwrap();

        TestEnv { bare, dev_a, dev_b }
    }

    pub fn bare_url(&self) -> String {
        self.bare.path().to_string_lossy().to_string()
    }

    pub fn init(&self, dev: &FakeDevice) -> (i32, String) {
        run(
            dev,
            &[
                "init",
                "--remote",
                &self.bare_url(),
                "--device",
                dev.name,
                "--passphrase-env",
                "XSYNC_PASSPHRASE",
            ],
        )
    }
}

pub fn run(dev: &FakeDevice, args: &[&str]) -> (i32, String) {
    run_env(dev, args, &[])
}

pub fn run_env(dev: &FakeDevice, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_claude-xsync"));
    cmd.args(args)
        .env("XSYNC_HOME", dev.home.path())
        .env("XSYNC_CLAUDE_DIR", dev.claude())
        .env("XSYNC_DIR", dev.xsync())
        .env("XSYNC_PASSPHRASE", "test-pass");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to spawn claude-xsync binary");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn init_and_first_push_populates_remote() {
    let env = TestEnv::new();
    let (code, out) = env.init(&env.dev_a);
    assert_eq!(code, 0, "init failed: {out}");
    let (code, out) = run(&env.dev_a, &["push"]);
    assert_eq!(code, 0, "push failed: {out}");
    assert!(out.contains("synced"), "missing summary: {out}");
    // second push = no-op (hash skip)
    let (code2, out2) = run(&env.dev_a, &["push"]);
    assert_eq!(code2, 0, "second push failed: {out2}");
    assert!(
        out2.contains("✓ 0 synced"),
        "expected no-op summary: {out2}"
    );
}

fn find_conflict_file(dir: &std::path::Path, base: &str) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name.starts_with(&format!("{base}.xsync-conflict."))
            .then(|| e.path())
    })
}

fn bare_commit_count(env: &TestEnv) -> String {
    let out = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(env.bare.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn full_cross_device_roundtrip_with_path_rewrite() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // projects dir renamed to dev_b's encoded home
    let proj = env
        .dev_b
        .claude()
        .join(format!("projects/{}-ws-app", env.dev_b.enc_home()));
    let content = fs::read_to_string(proj.join("s.jsonl")).unwrap();
    assert!(
        content.contains(&format!("{}/ws/app", env.dev_b.home_str())),
        "cwd not rewritten to dev_b home: {content}"
    );
    assert!(
        !content.contains(&env.dev_a.home_str()),
        "dev_a home leaked: {content}"
    );
}

#[test]
fn forgot_push_scenario_c3_no_deadlock_no_loss() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // B: edit settings + append a history line, push
    fs::write(
        env.dev_b.claude().join("settings.json"),
        b"{\"model\":\"haiku\"}",
    )
    .unwrap();
    let mut hb = fs::read(env.dev_b.claude().join("history.jsonl")).unwrap();
    hb.extend_from_slice(b"{\"display\":\"cmd-b\"}\n");
    fs::write(env.dev_b.claude().join("history.jsonl"), &hb).unwrap();
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // A forgot to push: local-only new file + modified history + modified settings
    fs::write(env.dev_a.claude().join("CLAUDE.md"), b"# my rules\n").unwrap();
    let mut ha = fs::read(env.dev_a.claude().join("history.jsonl")).unwrap();
    ha.extend_from_slice(b"{\"display\":\"cmd-a2\"}\n");
    fs::write(env.dev_a.claude().join("history.jsonl"), &ha).unwrap();
    fs::write(
        env.dev_a.claude().join("settings.json"),
        b"{\"model\":\"opus-4.8\"}",
    )
    .unwrap();

    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "pull must complete: {o}");

    // local-only new file preserved
    assert_eq!(
        fs::read(env.dev_a.claude().join("CLAUDE.md")).unwrap(),
        b"# my rules\n"
    );
    // history = line union (remote lines + local-only lines)
    let h = fs::read_to_string(env.dev_a.claude().join("history.jsonl")).unwrap();
    for needle in ["cmd-a", "cmd-b", "cmd-a2"] {
        assert!(h.contains(needle), "history union missing {needle}: {h}");
    }
    // settings = remote version, local version kept as conflict copy
    assert_eq!(
        fs::read(env.dev_a.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"haiku\"}"
    );
    let conflict =
        find_conflict_file(&env.dev_a.claude(), "settings.json").expect("conflict copy must exist");
    assert_eq!(fs::read(conflict).unwrap(), b"{\"model\":\"opus-4.8\"}");
}

#[test]
fn verbatim_mode_files_skip_resolve() {
    let env = TestEnv::new();
    // unknown-format file: bytes must survive the round trip untouched
    let payload = format!("BIN\x00 {}/ws/app \x01", env.dev_a.home_str()).into_bytes();
    fs::create_dir_all(env.dev_a.claude().join("skills")).unwrap();
    fs::write(env.dev_a.claude().join("skills/tool.bin"), &payload).unwrap();

    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 1, "verbatim degrade is a warning exit: {o}");
    assert!(o.contains("verbatim"), "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("skills/tool.bin")).unwrap(),
        payload,
        "verbatim file must be byte-identical (no resolve applied)"
    );
}

#[test]
fn squash_recovery() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // two more pushes from A
    for v in ["v1", "v2"] {
        fs::write(
            env.dev_a.claude().join("settings.json"),
            format!("{{\"model\":\"{v}\"}}"),
        )
        .unwrap();
        let (c, o) = run(&env.dev_a, &["push"]);
        assert_eq!(c, 0, "{o}");
    }

    // squash history into a single commit
    let (c, o) = run(&env.dev_a, &["gc", "--squash"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        bare_commit_count(&env),
        "1",
        "history must be a single commit"
    );

    // B pull auto-recovers from the rewritten history
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "divergence recovery must be automatic: {o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"v2\"}"
    );
}

#[test]
fn rekey_reencrypts_and_squashes_old_key_out() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // rekey on A with a new passphrase
    let (c, o) = run_env(
        &env.dev_a,
        &["rekey"],
        &[("XSYNC_NEW_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 0, "rekey failed: {o}");

    // old-key history is gone: single commit
    assert_eq!(bare_commit_count(&env), "1");

    // old passphrase must now fail closed
    let (c, o) = run(&env.dev_b, &["status"]);
    assert_eq!(c, 2, "old passphrase must fail: {o}");

    // a fresh device with the NEW passphrase can init + pull
    let dev_c = FakeDevice::new("linux");
    fs::create_dir_all(dev_c.claude()).unwrap();
    let (c, o) = run_env(
        &dev_c,
        &[
            "init",
            "--remote",
            &env.bare_url(),
            "--device",
            "linux",
            "--passphrase-env",
            "XSYNC_PASSPHRASE",
        ],
        &[("XSYNC_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 0, "init with new passphrase failed: {o}");
    let (c, o) = run_env(&dev_c, &["pull"], &[("XSYNC_PASSPHRASE", "new-pass")]);
    assert_eq!(c, 0, "pull with new passphrase failed: {o}");
    assert_eq!(
        fs::read(dev_c.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"opus\"}"
    );
}

#[test]
fn guard_a_blocks_out_of_order_push() {
    let env = TestEnv::new();
    let (code, out) = env.init(&env.dev_a);
    assert_eq!(code, 0, "init a failed: {out}");
    let (code, out) = env.init(&env.dev_b);
    assert_eq!(code, 0, "init b failed: {out}");

    // dev_b pushes first
    let (code, out) = run(&env.dev_b, &["push"]);
    assert_eq!(code, 0, "push b failed: {out}");

    // dev_a (stale) push must be blocked with a "pull first" hint
    let (code, out) = run(&env.dev_a, &["push"]);
    assert_eq!(code, 2, "expected guard A abort: {out}");
    assert!(out.contains("pull"), "expected pull hint: {out}");
}
