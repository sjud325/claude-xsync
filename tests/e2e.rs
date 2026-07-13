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
        FakeDevice { home: TempDir::new().unwrap(), name }
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
    pub fn new() -> TestEnv {
        let bare = TempDir::new().unwrap();
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(bare.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let dev_a = FakeDevice::new("mac");
        let dev_b = FakeDevice::new("win");

        // dev_a: one session with a cwd line + settings.json
        let proj = dev_a.claude().join(format!("projects/{}-ws-app", dev_a.enc_home()));
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("s.jsonl"),
            format!("{{\"cwd\":\"{}/ws/app\",\"type\":\"user\"}}\n", dev_a.home_str()),
        )
        .unwrap();
        fs::write(dev_a.claude().join("settings.json"), b"{\"model\":\"opus\"}").unwrap();

        // dev_b: different settings
        fs::create_dir_all(dev_b.claude()).unwrap();
        fs::write(dev_b.claude().join("settings.json"), b"{\"model\":\"sonnet\"}").unwrap();

        TestEnv { bare, dev_a, dev_b }
    }

    pub fn bare_url(&self) -> String {
        self.bare.path().to_string_lossy().to_string()
    }

    pub fn init(&self, dev: &FakeDevice) -> (i32, String) {
        run(dev, &[
            "init",
            "--remote", &self.bare_url(),
            "--device", dev.name,
            "--passphrase-env", "XSYNC_PASSPHRASE",
        ])
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
    assert!(out2.contains("✓ 0 synced"), "expected no-op summary: {out2}");
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
