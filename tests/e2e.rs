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
                json_escape(&dev_a.home_str())
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

/// Escape a path for embedding inside a JSON string literal — Windows homes
/// contain backslashes, which are JSON escape characters.
pub fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
}

/// The forward-slash form pull-resolve emits on every OS.
pub fn slash_form(s: &str) -> String {
    s.replace('\\', "/")
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
    // pull-resolve emits the forward-slash form on every OS
    assert!(
        content.contains(&format!("{}/ws/app", slash_form(&env.dev_b.home_str()))),
        "cwd not rewritten to dev_b home: {content}"
    );
    assert!(
        !content.contains(&env.dev_a.home_str())
            && !content.contains(&slash_form(&env.dev_a.home_str())),
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
fn quoted_peer_home_absorbed_once_and_syncs() {
    // Spec §12.2 (C′): a session quoting the peer's home (canonical case)
    // syncs to that peer; the quote is absorbed as a live path exactly once,
    // after which everything is a stable fixpoint.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let rel = "plans/quoted.jsonl";
    let original = format!(
        "{{\"note\":\"peer log said {}/x\"}}\n{{\"note\":\"DATA-KEEP\"}}\n",
        json_escape(&env.dev_b.home_str())
    );
    fs::create_dir_all(env.dev_a.claude().join("plans")).unwrap();
    fs::write(env.dev_a.claude().join(rel), original.as_bytes()).unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // B pull: file APPLIES (no skip) with the quote byte-intact, plus notice
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "pull must apply the quoted file: {o}");
    assert!(o.contains("absorbed"), "expected absorption notice: {o}");
    assert_eq!(
        fs::read_to_string(env.dev_b.claude().join(rel)).unwrap(),
        original,
        "quote must arrive byte-intact on first hop"
    );

    // B push: re-normalization tokenizes the absorbed quote and converges
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // A pull: the quote has morphed ONCE into A's live home; data intact
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let after = fs::read_to_string(env.dev_a.claude().join(rel)).unwrap();
    assert!(
        after.contains(&format!("{}/x", slash_form(&env.dev_a.home_str()))),
        "morph: {after}"
    );
    assert!(after.contains("DATA-KEEP"), "{after}");

    // converged: both sides now no-op
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("✓ 0 synced"), "must be a fixpoint now: {o}");
}

#[test]
fn variant_case_quote_absorbs_with_canonicalization() {
    // Spec §12.2: absorption requires the RE-TOKENIZED form to be a fixpoint,
    // not byte-identity of the quote itself — so case/separator variants of
    // the peer home also absorb (intact on first hop, canonicalized after).
    // This is what makes backslash-form Windows quotes sync on the real
    // Windows machine.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let rel = "plans/case-variant.jsonl";
    let original = format!(
        "{{\"note\":\"peer log said {}/x\"}}\n{{\"note\":\"CASE-KEEP\"}}\n",
        json_escape(&env.dev_b.home_str().to_uppercase())
    );
    fs::create_dir_all(env.dev_a.claude().join("plans")).unwrap();
    fs::write(env.dev_a.claude().join(rel), original.as_bytes()).unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "variant-case quote must absorb: {o}");
    assert!(o.contains("absorbed"), "expected absorption notice: {o}");
    assert_eq!(
        fs::read_to_string(env.dev_b.claude().join(rel)).unwrap(),
        original,
        "first hop stays byte-intact"
    );

    // round trip canonicalizes the quote's case
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let after = fs::read_to_string(env.dev_a.claude().join(rel)).unwrap();
    assert!(
        after.contains(&format!("{}/x", slash_form(&env.dev_a.home_str()))),
        "canonicalized morph: {after}"
    );
    assert!(after.contains("CASE-KEEP"), "{after}");
}

#[test]
fn sync_survives_autocrlf_git_config() {
    // Windows Git defaults to core.autocrlf=true; git classifies NUL-free
    // blobs as text, and small age ciphertexts are NUL-free ~30% of the
    // time — so checkout/add newline translation corrupts objects unless the
    // repo defends itself. Force the hostile config on every git the binary
    // spawns and require a full round trip to survive it.
    let env = TestEnv::new();
    let gitcfg = env.bare.path().join("gitconfig-autocrlf");
    fs::write(&gitcfg, "[core]\n\tautocrlf = true\n").unwrap();
    let cfg = gitcfg.to_string_lossy().to_string();
    let hostile: &[(&str, &str)] = &[("GIT_CONFIG_GLOBAL", cfg.as_str())];

    let init_a = [
        "init",
        "--remote",
        &env.bare_url(),
        "--device",
        "mac",
        "--passphrase-env",
        "XSYNC_PASSPHRASE",
    ];
    assert_eq!(run_env(&env.dev_a, &init_a, hostile).0, 0);
    let (c, o) = run_env(&env.dev_a, &["push"], hostile);
    assert_eq!(c, 0, "{o}");

    // Plant a canary that git's content sniffing WILL classify as text —
    // small age objects hit this ~0.3% of the time (mostly-printable header,
    // tiny ciphertext), which is exactly the Windows CI flake. The canary
    // makes the failure deterministic on every OS.
    let canary = b"age-canary: printable header line\nsecond line, still printable\n";
    {
        let stage = TempDir::new().unwrap();
        let clone = stage.path().join("clone");
        let clone_s = clone.to_string_lossy().to_string();
        let git = |args: &[&str]| {
            let out = Command::new("git").args(args).output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["clone", &env.bare_url(), clone_s.as_str()]);
        fs::write(clone.join("objects/zz-canary.age"), canary).unwrap();
        git(&["-C", clone_s.as_str(), "add", "-A"]);
        git(&[
            "-C",
            clone_s.as_str(),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "canary",
        ]);
        git(&["-C", clone_s.as_str(), "push", "origin", "main"]);
    }

    let init_b = [
        "init",
        "--remote",
        &env.bare_url(),
        "--device",
        "win",
        "--passphrase-env",
        "XSYNC_PASSPHRASE",
    ];
    assert_eq!(run_env(&env.dev_b, &init_b, hostile).0, 0);
    // the binary's own clone must deliver every object byte-identical even
    // under a hostile global autocrlf=true
    assert_eq!(
        fs::read(env.dev_b.xsync().join("repo/objects/zz-canary.age")).unwrap(),
        canary.to_vec(),
        "git newline translation corrupted an object in the sync repo clone"
    );
    let (c, o) = run_env(&env.dev_b, &["pull"], hostile);
    assert_eq!(c, 0, "pull must survive autocrlf: {o}");
    assert!(!o.contains("skipping"), "no object may corrupt: {o}");

    // B edits + pushes, A pulls — the same round trip the CI flake died on
    fs::write(
        env.dev_b.claude().join("settings.json"),
        b"{\"model\":\"b-edit\"}",
    )
    .unwrap();
    let (c, o) = run_env(&env.dev_b, &["push"], hostile);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run_env(&env.dev_a, &["pull"], hostile);
    assert_eq!(c, 0, "pull after peer push must survive autocrlf: {o}");
    assert!(!o.contains("skipping"), "no object may corrupt: {o}");
    assert_eq!(
        fs::read(env.dev_a.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"b-edit\"}"
    );
}

fn set_mtime(path: &std::path::Path, unix_secs: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs))
        .unwrap();
}

fn mtime_secs(path: &std::path::Path) -> u64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn pull_preserves_original_mtimes() {
    // `claude --resume` orders sessions by file mtime; a pull that stamps
    // everything with "now" collapses the whole history into one moment.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let session = env
        .dev_a
        .claude()
        .join(format!("projects/{}-ws-app/s.jsonl", env.dev_a.enc_home()));
    let old = 1_700_000_000u64;
    set_mtime(&session, old);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let applied = env
        .dev_b
        .claude()
        .join(format!("projects/{}-ws-app/s.jsonl", env.dev_b.enc_home()));
    let got = mtime_secs(&applied);
    assert!(
        got.abs_diff(old) <= 2,
        "mtime must survive the round trip: got {got}, want ~{old}"
    );
}

#[test]
fn pull_repairs_drifted_mtimes_on_in_sync_files() {
    // Files pulled by older versions carry pull-time mtimes; a later pull
    // must repair them from the manifest without rewriting content.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let session = env
        .dev_a
        .claude()
        .join(format!("projects/{}-ws-app/s.jsonl", env.dev_a.enc_home()));
    let old = 1_700_000_000u64;
    set_mtime(&session, old);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // simulate the old-version damage: mtime drifted to "now"
    let applied = env
        .dev_b
        .claude()
        .join(format!("projects/{}-ws-app/s.jsonl", env.dev_b.enc_home()));
    set_mtime(&applied, 1_800_000_000);

    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let got = mtime_secs(&applied);
    assert!(
        got.abs_diff(old) <= 2,
        "in-sync pull must repair drifted mtime: got {got}, want ~{old}"
    );
}

#[test]
fn conflict_copies_are_never_collected_for_push() {
    // Real-machine find (2026-07-17, third device): .xsync-conflict copies
    // created inside wholesale-synced dirs (plugins/, projects/) were being
    // picked up by the next push and propagated to every device; top-level
    // copies spammed the unknown-entry warning.
    let env = TestEnv::new();
    fs::create_dir_all(env.dev_a.claude().join("plugins")).unwrap();
    fs::write(
        env.dev_a.claude().join("plugins/marketplaces.json"),
        b"{\"a\":1}",
    )
    .unwrap();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // dev_b pre-exists with DIFFERENT copies of the same files → conflicts
    fs::create_dir_all(env.dev_b.claude().join("plugins")).unwrap();
    fs::write(
        env.dev_b.claude().join("plugins/marketplaces.json"),
        b"{\"b\":2}",
    )
    .unwrap();
    let projb = env
        .dev_b
        .claude()
        .join(format!("projects/{}-ws-app", env.dev_b.enc_home()));
    fs::create_dir_all(&projb).unwrap();
    fs::write(projb.join("s.jsonl"), b"{\"local\":\"divergent\"}\n").unwrap();
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("conflict"), "{o}");

    // the conflict copies exist on disk…
    let has_copy = |dir: &std::path::Path| {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".xsync-conflict."))
    };
    assert!(has_copy(&env.dev_b.claude().join("plugins")));
    assert!(has_copy(&projb));
    assert!(has_copy(&env.dev_b.claude())); // settings.json conflict at top level

    // …but a push must neither ship them nor warn about them
    let (c, o) = run(&env.dev_b, &["push", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("xsync-conflict"),
        "conflict copies leaked into push: {o}"
    );
}

#[test]
fn init_inserts_multi_device_note_on_first_device_only() {
    let env = TestEnv::new();
    // first device (empty remote): note inserted
    assert_eq!(env.init(&env.dev_a).0, 0);
    let a_md = fs::read_to_string(env.dev_a.claude().join("CLAUDE.md")).unwrap();
    assert!(a_md.contains("claude-xsync:multi-device:begin"), "{a_md}");
    // the block must contain no literal home paths — the path transform
    // would localize them on the peer and corrupt the text
    assert!(!a_md.contains(&env.dev_a.home_str()));

    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // second device (remote already has a manifest): init must NOT create a
    // local CLAUDE.md — the note arrives via pull instead, conflict-free
    assert_eq!(env.init(&env.dev_b).0, 0);
    assert!(!env.dev_b.claude().join("CLAUDE.md").exists());
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let b_md = fs::read_to_string(env.dev_b.claude().join("CLAUDE.md")).unwrap();
    assert!(b_md.contains("claude-xsync:multi-device:begin"), "{b_md}");
}

#[test]
fn claude_md_command_appends_preserving_content_idempotently() {
    let env = TestEnv::new();
    fs::write(env.dev_a.claude().join("CLAUDE.md"), "# my rules\n").unwrap();
    let (c, o) = run(&env.dev_a, &["claude-md"]);
    assert_eq!(c, 0, "{o}");
    let md = fs::read_to_string(env.dev_a.claude().join("CLAUDE.md")).unwrap();
    assert!(md.starts_with("# my rules"), "user content clobbered: {md}");
    assert!(md.contains("claude-xsync:multi-device:begin"), "{md}");

    // idempotent: second run changes nothing
    let (c, o) = run(&env.dev_a, &["claude-md"]);
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("already"), "{o}");
    let md2 = fs::read_to_string(env.dev_a.claude().join("CLAUDE.md")).unwrap();
    assert_eq!(md, md2);
}

#[test]
fn init_no_claude_md_opts_out() {
    let env = TestEnv::new();
    let url = env.bare_url();
    let (c, o) = run(
        &env.dev_a,
        &[
            "init",
            "--remote",
            &url,
            "--device",
            "mac",
            "--passphrase-env",
            "XSYNC_PASSPHRASE",
            "--no-claude-md",
        ],
    );
    assert_eq!(c, 0, "{o}");
    assert!(!env.dev_a.claude().join("CLAUDE.md").exists());
}

#[test]
fn pull_auto_runs_app_index_when_config_enabled() {
    let env = TestEnv::new();
    // dev_a has one UUID-named session to sync
    let proj = env
        .dev_a
        .claude()
        .join(format!("projects/{}-ws-app", env.dev_a.enc_home()));
    fs::create_dir_all(&proj).unwrap();
    let sid = "44444444-4444-4444-8444-444444444444";
    fs::write(
        proj.join(format!("{sid}.jsonl")),
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"자동 인덱스 테스트\"}},\"timestamp\":\"2020-01-01T00:00:00.000Z\",\"cwd\":\"{}/ws/app\",\"sessionId\":\"{sid}\"}}\n",
            json_escape(&env.dev_a.home_str())
        ),
    )
    .unwrap();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // dev_b opts in via config
    assert_eq!(env.init(&env.dev_b).0, 0);
    let cfg_path = env.dev_b.xsync().join("config.toml");
    let cfg_text = fs::read_to_string(&cfg_path).unwrap().replace(
        "app_index_after_pull = false",
        "app_index_after_pull = true",
    );
    assert!(cfg_text.contains("app_index_after_pull = true"));
    fs::write(&cfg_path, cfg_text).unwrap();

    let appdir = env.dev_b.home.path().join("appdata/acc/org");
    fs::create_dir_all(&appdir).unwrap();
    let appdir_s = appdir.to_string_lossy().to_string();
    let (c, o) = run_env(
        &env.dev_b,
        &["pull"],
        &[("XSYNC_APP_SESSIONS_DIR", appdir_s.as_str())],
    );
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("indexed 1 sessions"), "{o}");
    let created: Vec<_> = fs::read_dir(&appdir).unwrap().flatten().collect();
    assert_eq!(created.len(), 1, "{o}");

    // without the flag (dev_a pulling) nothing app-related happens — every
    // other pull e2e in this suite implicitly covers the silent-skip path
}

#[test]
fn app_index_creates_entries_for_unindexed_sessions() {
    // The desktop app lists only sessions that have a local_*.json entry in
    // its private index — synced .jsonl files alone never appear. app-index
    // backfills entries for top-level sessions, template-cloning a native
    // entry so platform-specific fields carry over.
    let env = TestEnv::new();
    let appdir = env.dev_a.home.path().join("appdata/acc-uuid/org-uuid");
    fs::create_dir_all(&appdir).unwrap();

    let proj = env
        .dev_a
        .claude()
        .join(format!("projects/{}-ws-app", env.dev_a.enc_home()));
    fs::create_dir_all(&proj).unwrap();
    let x = "11111111-1111-4111-8111-111111111111"; // already indexed
    let y = "22222222-2222-4222-8222-222222222222"; // needs an entry
    let home = json_escape(&env.dev_a.home_str());
    for (id, title_line) in [
        (x, String::new()),
        (
            y,
            format!("{{\"type\":\"custom-title\",\"customTitle\":\"와이 세션\",\"sessionId\":\"{y}\"}}\n"),
        ),
    ] {
        let mut s = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"첫 질문\"}},\"timestamp\":\"2020-01-01T00:00:00.000Z\",\"cwd\":\"{home}/ws/app\",\"sessionId\":\"{id}\"}}\n"
        );
        s.push_str(&title_line);
        s.push_str(&format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\"}},\"timestamp\":\"2020-01-02T03:04:05.678Z\",\"cwd\":\"{home}/ws/app\",\"sessionId\":\"{id}\"}}\n"
        ));
        fs::write(proj.join(format!("{id}.jsonl")), s).unwrap();
    }
    // subagent transcripts must never be indexed
    fs::create_dir_all(proj.join(format!("{y}/subagents"))).unwrap();
    fs::write(
        proj.join(format!("{y}/subagents/agent-abc.jsonl")),
        "{\"type\":\"user\",\"timestamp\":\"2020-01-01T00:00:00.000Z\"}\n",
    )
    .unwrap();

    // native template entry for X (session-specific fields must be reset)
    fs::write(
        appdir.join("local_aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.json"),
        format!(
            "{{\"sessionId\":\"local_aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\",\"cliSessionId\":\"{x}\",\"cwd\":\"{home}/ws/app\",\"originCwd\":\"{home}/ws/app\",\"title\":\"X native\",\"titleSource\":\"auto\",\"isArchived\":false,\"createdAt\":1,\"lastActivityAt\":2,\"model\":\"claude-test-model\",\"completedTurns\":7,\"writtenBranches\":[\"main\"]}}"
        ),
    )
    .unwrap();

    let appdir_s = appdir.to_string_lossy().to_string();
    let hostile: &[(&str, &str)] = &[("XSYNC_APP_SESSIONS_DIR", appdir_s.as_str())];
    let (c, o) = run_env(&env.dev_a, &["app-index"], hostile);
    assert_eq!(c, 0, "{o}");

    let entries: Vec<_> = fs::read_dir(&appdir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("local_"))
        .collect();
    assert_eq!(entries.len(), 2, "exactly one new entry: {o}");

    let new_entry = entries
        .iter()
        .find(|e| !e.file_name().to_string_lossy().contains("aaaaaaaa"))
        .expect("new index file");
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(new_entry.path()).unwrap()).unwrap();
    assert_eq!(v["cliSessionId"], y);
    assert_eq!(
        format!("{}.json", v["sessionId"].as_str().unwrap()),
        new_entry.file_name().to_string_lossy()
    );
    assert_eq!(v["title"], "와이 세션");
    assert_eq!(v["titleSource"], "custom");
    assert_eq!(v["createdAt"], 1_577_836_800_000u64);
    assert_eq!(v["lastActivityAt"], 1_577_934_245_678u64);
    assert_eq!(v["model"], "claude-test-model"); // inherited from template
    assert_eq!(v["completedTurns"], 0); // session-specific fields reset
    assert_eq!(v["writtenBranches"], serde_json::json!([]));
    assert_eq!(v["isArchived"], false);
    // the app stores cwd in the platform's NATIVE separator form
    let expected_cwd_tail = if cfg!(windows) {
        "\\ws\\app"
    } else {
        "/ws/app"
    };
    assert!(
        v["cwd"].as_str().unwrap().ends_with(expected_cwd_tail),
        "{v}"
    );

    // idempotent: second run creates nothing
    let (c, o) = run_env(&env.dev_a, &["app-index"], hostile);
    assert_eq!(c, 0, "{o}");
    let count = fs::read_dir(&appdir).unwrap().flatten().count();
    assert_eq!(count, 2, "second run must be a no-op: {o}");
}

#[test]
fn app_index_without_template_creates_minimal_entry() {
    let env = TestEnv::new();
    let appdir = env.dev_a.home.path().join("appdata/acc-uuid/org-uuid");
    fs::create_dir_all(&appdir).unwrap();
    let proj = env
        .dev_a
        .claude()
        .join(format!("projects/{}-ws-app", env.dev_a.enc_home()));
    fs::create_dir_all(&proj).unwrap();
    let y = "33333333-3333-4333-8333-333333333333";
    fs::write(
        proj.join(format!("{y}.jsonl")),
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"제목 없는 세션의 첫 메시지\"}},\"timestamp\":\"2020-01-01T00:00:00.000Z\",\"cwd\":\"{}/ws/app\",\"sessionId\":\"{y}\"}}\n",
            json_escape(&env.dev_a.home_str())
        ),
    )
    .unwrap();

    let appdir_s = appdir.to_string_lossy().to_string();
    let (c, o) = run_env(
        &env.dev_a,
        &["app-index"],
        &[("XSYNC_APP_SESSIONS_DIR", appdir_s.as_str())],
    );
    assert_eq!(c, 0, "{o}");
    let entry = fs::read_dir(&appdir)
        .unwrap()
        .flatten()
        .next()
        .expect("entry");
    let v: serde_json::Value = serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
    assert_eq!(v["cliSessionId"], y);
    // fallback title = first user message
    assert_eq!(v["title"], "제목 없는 세션의 첫 메시지");
    assert_eq!(v["createdAt"], 1_577_836_800_000u64);
    assert_eq!(v["isArchived"], false);
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
fn pull_skip_must_not_cascade_into_push_clobber() {
    // Review C1: a file the puller cannot stage (here: object corrupted on
    // the remote) must not let the puller's next push overwrite the newer
    // remote version with its stale local copy.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let rel = "plans/notes.jsonl";
    fs::create_dir_all(env.dev_a.claude().join("plans")).unwrap();
    fs::write(env.dev_a.claude().join(rel), b"{\"note\":\"v1 plain\"}\n").unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}"); // v1 lands on B

    // A pushes a newer v2
    let v2 = b"{\"note\":\"v2\"}\n{\"note\":\"IMPORTANT-V2\"}\n";
    fs::write(env.dev_a.claude().join(rel), v2).unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // corrupt every object on the remote so B's staging of v2 fails
    let tmp = TempDir::new().unwrap();
    let clone = tmp.path().join("clone");
    let clone_s = clone.to_string_lossy().to_string();
    let out = Command::new("git")
        .args(["clone", &env.bare_url(), clone_s.as_str()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for e in fs::read_dir(clone.join("objects")).unwrap().flatten() {
        let mut bytes = fs::read(e.path()).unwrap();
        bytes.extend_from_slice(b"CORRUPT");
        fs::write(e.path(), bytes).unwrap();
    }
    for args in [
        vec!["-C", clone_s.as_str(), "add", "-A"],
        vec![
            "-C",
            clone_s.as_str(),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "tamper",
        ],
        vec!["-C", clone_s.as_str(), "push", "origin", "main"],
    ] {
        let out = Command::new("git").args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // B pull: that file is skipped (exit 1), B keeps v1 locally
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 1, "expected skip warning exit: {o}");
    assert!(o.contains("skipping"), "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join(rel)).unwrap(),
        b"{\"note\":\"v1 plain\"}\n"
    );

    // B pushes an unrelated edit — it must NOT clobber the newer remote file
    fs::write(
        env.dev_b.claude().join("settings.json"),
        b"{\"model\":\"b-edit\"}",
    )
    .unwrap();
    let (_, o) = run(&env.dev_b, &["push"]);
    assert!(!o.contains("panicked"), "push must not crash: {o}");

    // A pull: A's newer content must survive the round trip
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let after = fs::read_to_string(env.dev_a.claude().join(rel)).unwrap();
    assert!(
        after.contains("IMPORTANT-V2"),
        "newer remote content was clobbered by a stale push: {after}"
    );
}

#[test]
fn rekey_on_stale_device_aborts_with_pull_hint() {
    // Review C2: rekey rebuilds the manifest from LOCAL plaintext and purges
    // history — running it on a device that hasn't pulled the peer's latest
    // push would destroy that data. It must abort like guard A.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // B pushes new data that A has not pulled
    fs::write(
        env.dev_b.claude().join("settings.json"),
        b"{\"model\":\"b-only\"}",
    )
    .unwrap();
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // stale A: rekey must refuse
    let (c, o) = run_env(
        &env.dev_a,
        &["rekey"],
        &[("XSYNC_NEW_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 2, "stale rekey must abort: {o}");
    assert!(o.contains("pull"), "expected pull hint: {o}");
    // history untouched, old passphrase still valid
    assert_ne!(bare_commit_count(&env), "1");
    let (c, o) = run(&env.dev_b, &["status"]);
    assert_eq!(c, 0, "old passphrase must still work: {o}");

    // after pulling, rekey goes through
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run_env(
        &env.dev_a,
        &["rekey"],
        &[("XSYNC_NEW_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 0, "anchored rekey must succeed: {o}");
    assert_eq!(bare_commit_count(&env), "1");
}

#[test]
fn history_rewrite_race_preserves_local_as_conflict() {
    // Review I3: if a device's push loses a race against a peer's squash
    // force-push, its content must surface as a conflict copy on the next
    // pull — never a silent revert.
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    let pre = {
        let out = Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(env.bare.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // B pushes v2
    fs::write(
        env.dev_b.claude().join("settings.json"),
        b"{\"model\":\"b-v2\"}",
    )
    .unwrap();
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // simulate a peer squash based on `pre`, force-pushed AFTER B's push
    let racer = TempDir::new().unwrap();
    let racer_repo = racer.path().join("clone");
    for args in [
        vec!["clone", &env.bare_url(), racer_repo.to_str().unwrap()],
        vec![
            "-C",
            racer_repo.to_str().unwrap(),
            "reset",
            "--hard",
            pre.as_str(),
        ],
        vec![
            "-C",
            racer_repo.to_str().unwrap(),
            "checkout",
            "--orphan",
            "raced",
        ],
        vec!["-C", racer_repo.to_str().unwrap(), "add", "-A"],
        vec![
            "-C",
            racer_repo.to_str().unwrap(),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "raced-squash",
        ],
        vec!["-C", racer_repo.to_str().unwrap(), "branch", "-M", "main"],
        vec![
            "-C",
            racer_repo.to_str().unwrap(),
            "push",
            "--force",
            "origin",
            "main",
        ],
    ] {
        let out = Command::new("git").args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // B pull: rewritten remote wins the file, but B's v2 must survive visibly
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"opus\"}"
    );
    let mut found = false;
    for e in fs::read_dir(env.dev_b.claude()).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("settings.json.xsync-conflict.") {
            let body = fs::read(e.path()).unwrap();
            if body == b"{\"model\":\"b-v2\"}" {
                found = true;
            }
        }
    }
    assert!(found, "b-v2 must be preserved as a conflict copy: {o}");
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

#[test]
fn pull_dry_run_summary_counts_planned_changes() {
    let env = TestEnv::new();
    let (code, out) = env.init(&env.dev_a);
    assert_eq!(code, 0, "init a failed: {out}");
    let (code, out) = run(&env.dev_a, &["push"]);
    assert_eq!(code, 0, "push a failed: {out}");
    let (code, out) = env.init(&env.dev_b);
    assert_eq!(code, 0, "init b failed: {out}");

    // dev_a shipped s.jsonl + history.jsonl + settings.json + the CLAUDE.md
    // note; dev_b's own settings.json differs (conflict). The summary must
    // count what WOULD be applied, matching the "would apply" lines above it.
    let (code, out) = run(&env.dev_b, &["pull", "--dry-run"]);
    assert_eq!(code, 0, "dry-run pull failed: {out}");
    assert!(
        out.contains("would apply"),
        "no planned files listed: {out}"
    );
    assert!(
        out.contains("✓ 4 synced"),
        "dry-run summary must count planned writes: {out}"
    );
    assert!(out.contains("⚡ 1 conflicts"), "conflict count lost: {out}");

    // dry-run must not have changed anything: the real pull sees the same plan
    let (code, out) = run(&env.dev_b, &["pull"]);
    assert_eq!(code, 0, "real pull failed: {out}");
    assert!(
        out.contains("✓ 4 synced"),
        "real pull disagrees with dry-run count: {out}"
    );
}

#[test]
fn init_warns_on_duplicate_device_name() {
    let env = TestEnv::new();
    let (code, out) = env.init(&env.dev_a); // device "mac"
    assert_eq!(code, 0, "init a failed: {out}");
    let (code, out) = run(&env.dev_a, &["push"]);
    assert_eq!(code, 0, "push a failed: {out}");

    // a different machine claiming the same name silently defeats Guard A —
    // warn, but never block: re-init on the SAME machine is a recovery flow
    let (code, out) = run(
        &env.dev_b,
        &["init", "--remote", &env.bare_url(), "--device", "mac"],
    );
    assert_eq!(code, 0, "duplicate name must warn, not fail: {out}");
    assert!(
        out.contains("already used"),
        "missing duplicate-name warning: {out}"
    );

    // a unique name stays quiet
    let (code, out) = run(
        &env.dev_b,
        &["init", "--remote", &env.bare_url(), "--device", "win2"],
    );
    assert_eq!(code, 0, "init b failed: {out}");
    assert!(
        !out.contains("already used"),
        "false duplicate warning: {out}"
    );
}

#[test]
fn pull_prunes_old_backups_when_configured() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);

    // two stale backup dirs from long-gone pulls
    for stamp in ["1000000001", "1000000002"] {
        let d = env
            .dev_b
            .home
            .path()
            .join(format!(".claude.backup.{stamp}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("old.json"), b"{}").unwrap();
    }
    let count_backups = || {
        fs::read_dir(env.dev_b.home.path())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".claude.backup.")
            })
            .count()
    };

    // default (backup_keep = 0): pull creates a real backup (settings.json
    // conflict) and prunes nothing
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(!o.contains("pruned"), "must not prune by default: {o}");
    assert_eq!(
        count_backups(),
        3,
        "fakes + the real backup must all remain"
    );

    // opt in: keep only the 2 newest
    let cfg_path = env.dev_b.xsync().join("config.toml");
    let cfg = fs::read_to_string(&cfg_path).unwrap();
    assert!(
        cfg.contains("backup_keep = 0"),
        "init must write the knob explicitly: {cfg}"
    );
    fs::write(&cfg_path, cfg.replace("backup_keep = 0", "backup_keep = 2")).unwrap();

    // dry-run must never delete anything
    let (c, o) = run(&env.dev_b, &["pull", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(!o.contains("pruned"), "dry-run must not prune: {o}");
    assert_eq!(count_backups(), 3);

    // a real pull (even a no-op one) prunes down to the limit
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("pruned 1"), "missing prune report: {o}");
    assert_eq!(count_backups(), 2);
    assert!(
        !env.dev_b
            .home
            .path()
            .join(".claude.backup.1000000001")
            .exists(),
        "oldest fake must be gone"
    );
    assert!(
        env.dev_b
            .home
            .path()
            .join(".claude.backup.1000000002")
            .exists(),
        "second-newest must survive with keep = 2"
    );
}

#[test]
fn pull_never_overwrites_paths_outside_this_devices_sync_set() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    // dev_a opts the (normally machine-local) daemon dir into its sync set
    let a_cfg = env.dev_a.xsync().join("config.toml");
    let cfg = fs::read_to_string(&a_cfg).unwrap();
    fs::write(
        &a_cfg,
        cfg.replace("extra_paths = []", "extra_paths = [\"daemon\"]"),
    )
    .unwrap();
    fs::create_dir_all(env.dev_a.claude().join("daemon")).unwrap();
    fs::write(env.dev_a.claude().join("daemon/marker.txt"), b"from-a").unwrap();
    // guard against a silently no-oped config splice: the opt-in must be
    // visible in what push plans to send
    let (c, o) = run(&env.dev_a, &["push", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("daemon/marker.txt"),
        "config splice did not take effect — test would be vacuous: {o}"
    );
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // dev_b did NOT opt in — daemon is machine-local there, and its own
    // live marker must never be overwritten by the remote entry
    assert_eq!(env.init(&env.dev_b).0, 0);
    fs::create_dir_all(env.dev_b.claude().join("daemon")).unwrap();
    fs::write(env.dev_b.claude().join("daemon/marker.txt"), b"b-local").unwrap();

    let (c, o) = run(&env.dev_b, &["pull", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("daemon/marker.txt"),
        "dry-run must not plan writes outside this device's sync set: {o}"
    );
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("daemon/marker.txt")).unwrap(),
        b"b-local",
        "machine-local file was overwritten by a remote entry: {o}"
    );
    // the rest of the pull still applies normally
    assert!(
        env.dev_b.claude().join("history.jsonl").exists(),
        "synced files must still arrive: {o}"
    );
    // status must agree with what pull actually does — no phantom to-pull
    let (c, o) = run(&env.dev_b, &["status", "--offline"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("to pull: 0"),
        "status counts entries pull will never apply: {o}"
    );
}

#[test]
fn remote_deletion_of_desynced_path_leaves_local_file_untouched() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // simulate a 0.1.13-era install: the sweep marker was once synced
    // (anchored in state) but is machine-local since 0.1.16, and the peer's
    // upgrade deleted it from the remote
    fs::create_dir_all(env.dev_b.claude().join("plugins")).unwrap();
    fs::write(
        env.dev_b.claude().join("plugins/.last_inuse_sweep"),
        b"my-live-stamp",
    )
    .unwrap();
    let state_path = env.dev_b.xsync().join("state.json");
    let state = fs::read_to_string(&state_path).unwrap();
    assert!(
        state.contains("\"files\": {"),
        "state format drifted: {state}"
    );
    fs::write(
        &state_path,
        state.replace(
            "\"files\": {",
            "\"files\": {\n    \"plugins/.last_inuse_sweep\": \"00\",",
        ),
    )
    .unwrap();

    // dry-run must not claim it would remove a file it will not touch
    let (c, o) = run(&env.dev_b, &["pull", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("would remove plugins/.last_inuse_sweep"),
        "dry-run promises a removal that never happens: {o}"
    );
    assert!(
        o.contains("would forget sync state for plugins/.last_inuse_sweep"),
        "missing honest dry-run line: {o}"
    );

    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("plugins/.last_inuse_sweep")).unwrap(),
        b"my-live-stamp",
        "machine-local marker must survive the remote deletion: {o}"
    );
    assert!(
        o.contains("forgot sync state for plugins/.last_inuse_sweep"),
        "missing honest apply line: {o}"
    );
    let state = fs::read_to_string(&state_path).unwrap();
    assert!(
        !state.contains(".last_inuse_sweep"),
        "state entry must be dropped: {state}"
    );
}

/// Shared setup: dev_a syncs daemon/ via extra_paths; dev_b opts in, pulls
/// (gaining an anchor), then opts back out — the anchor is now stranded.
fn stranded_anchor_env() -> TestEnv {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    assert_eq!(env.init(&env.dev_b).0, 0);
    for dev in [&env.dev_a, &env.dev_b] {
        let p = dev.xsync().join("config.toml");
        let cfg = fs::read_to_string(&p).unwrap();
        let patched = cfg.replace("extra_paths = []", "extra_paths = [\"daemon\"]");
        assert_ne!(patched, cfg, "config splice no-oped: {cfg}");
        fs::write(&p, patched).unwrap();
    }
    fs::create_dir_all(env.dev_a.claude().join("daemon")).unwrap();
    fs::write(env.dev_a.claude().join("daemon/marker.txt"), b"from-a").unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("daemon/marker.txt")).unwrap(),
        b"from-a",
        "opt-in pull must deliver the file: {o}"
    );
    // dev_b opts back out — daemon is machine-local for it from now on
    let p = env.dev_b.xsync().join("config.toml");
    let cfg = fs::read_to_string(&p).unwrap();
    fs::write(
        &p,
        cfg.replace("extra_paths = [\"daemon\"]", "extra_paths = []"),
    )
    .unwrap();
    env
}

#[test]
fn optout_pull_forgets_anchor_and_later_push_spares_peer_data() {
    let env = stranded_anchor_env();

    // the opt-out pull drops dev_b's anchor and says so honestly
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("forgot sync state for daemon/marker.txt (not in this device's sync set)"),
        "missing anchor-disposal line: {o}"
    );
    assert!(
        env.dev_b.claude().join("daemon/marker.txt").exists(),
        "opt-out must not delete the local copy"
    );

    // with the anchor gone, dev_b's push must not touch the remote entry
    let (c, o) = run(&env.dev_b, &["push", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("would delete daemon/marker.txt"),
        "push still plans to delete the peer's opted-in data: {o}"
    );
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // dev_a's data survives the full round trip
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(!o.contains("removed daemon/marker.txt"), "{o}");
    assert_eq!(
        fs::read(env.dev_a.claude().join("daemon/marker.txt")).unwrap(),
        b"from-a",
        "peer's opted-in file was deleted by dev_b's opt-out: {o}"
    );
}

#[test]
fn optout_push_before_any_pull_never_deletes_peer_data() {
    let env = stranded_anchor_env();

    // upgrade-order hazard: dev_b pushes FIRST, stranded anchor still present
    let (c, o) = run(&env.dev_b, &["push", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("would delete daemon/marker.txt"),
        "push turns a stranded anchor into deleting the peer's data: {o}"
    );
    assert!(
        o.contains("would forget sync state for daemon/marker.txt"),
        "missing honest dry-run line for anchor disposal: {o}"
    );
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("forgot sync state for daemon/marker.txt"),
        "missing anchor-disposal line: {o}"
    );

    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_a.claude().join("daemon/marker.txt")).unwrap(),
        b"from-a",
        "peer's opted-in file was deleted by dev_b's push: {o}"
    );
}

#[test]
fn extra_paths_plugins_wholesale_roundtrip() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    assert_eq!(env.init(&env.dev_b).0, 0);
    for dev in [&env.dev_a, &env.dev_b] {
        let p = dev.xsync().join("config.toml");
        let cfg = fs::read_to_string(&p).unwrap();
        let patched = cfg.replace("extra_paths = []", "extra_paths = [\"plugins\"]");
        assert_ne!(patched, cfg, "config splice no-oped: {cfg}");
        fs::write(&p, patched).unwrap();
    }
    let plugins = env.dev_a.claude().join("plugins");
    fs::create_dir_all(plugins.join("repos/foo")).unwrap();
    fs::write(plugins.join("config.json"), b"{\"root\":1}").unwrap();
    fs::write(plugins.join("repos/foo/manifest.json"), b"{\"nested\":1}").unwrap();
    // structurally machine-local: excluded even under a wholesale opt-in
    fs::write(plugins.join(".last_inuse_sweep"), b"stamp-a").unwrap();

    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    let b_plugins = env.dev_b.claude().join("plugins");
    assert_eq!(
        fs::read(b_plugins.join("config.json")).unwrap(),
        b"{\"root\":1}"
    );
    assert_eq!(
        fs::read(b_plugins.join("repos/foo/manifest.json")).unwrap(),
        b"{\"nested\":1}",
        "wholesale opt-in must deliver nested plugin files: {o}"
    );
    assert!(
        !b_plugins.join(".last_inuse_sweep").exists(),
        "sweep marker must never sync, even under wholesale opt-in: {o}"
    );
    // and status agrees nothing is left over
    let (c, o) = run(&env.dev_b, &["status", "--offline"]);
    assert_eq!(c, 0, "{o}");
    assert!(o.contains("to pull: 0"), "phantom to-pull remains: {o}");
}

#[test]
fn removed_paths_plugins_opt_out_is_respected() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    // dev_a opts plugin manifests out entirely
    let p = env.dev_a.xsync().join("config.toml");
    let cfg = fs::read_to_string(&p).unwrap();
    let patched = cfg.replace("removed_paths = []", "removed_paths = [\"plugins\"]");
    assert_ne!(patched, cfg, "config splice no-oped: {cfg}");
    fs::write(&p, patched).unwrap();
    fs::create_dir_all(env.dev_a.claude().join("plugins")).unwrap();
    fs::write(env.dev_a.claude().join("plugins/config.json"), b"a-local").unwrap();

    // collection must respect the opt-out (single-predicate contract)
    let (c, o) = run(&env.dev_a, &["push", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("plugins/config.json"),
        "removed_paths opt-out ignored by plugin-manifest collection: {o}"
    );
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // a peer that DOES sync plugin manifests pushes its own copy
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    fs::create_dir_all(env.dev_b.claude().join("plugins")).unwrap();
    fs::write(env.dev_b.claude().join("plugins/config.json"), b"from-b").unwrap();
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // the opted-out device neither receives nor loses its machine-local copy
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_a.claude().join("plugins/config.json")).unwrap(),
        b"a-local",
        "opt-out device's plugins/config.json was touched: {o}"
    );
    // and its next push leaves the peer's remote copy alone
    let (c, o) = run(&env.dev_a, &["push", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("would delete plugins/config.json"),
        "opt-out device plans to delete the peer's data: {o}"
    );
}

#[test]
fn dry_run_pull_after_squash_leaves_state_untouched() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // remote history rewritten while dev_b holds now-stale anchors
    fs::write(
        env.dev_a.claude().join("settings.json"),
        b"{\"model\":\"v2\"}",
    )
    .unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_a, &["gc", "--squash"]);
    assert_eq!(c, 0, "{o}");

    let state_path = env.dev_b.xsync().join("state.json");
    let before = fs::read(&state_path).unwrap();
    let (c, o) = run(&env.dev_b, &["pull", "--dry-run"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(&state_path).unwrap(),
        before,
        "dry-run pull mutated state.json after a squash: {o}"
    );
    // the real pull still recovers normally afterwards
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"v2\"}",
        "post-squash recovery broken: {o}"
    );
}

#[test]
fn real_pull_after_dry_run_still_detects_history_rewrite() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // dev_b opts daemon/ in and uploads its own file
    assert_eq!(env.init(&env.dev_b).0, 0);
    let p = env.dev_b.xsync().join("config.toml");
    let cfg = fs::read_to_string(&p).unwrap();
    let patched = cfg.replace("extra_paths = []", "extra_paths = [\"daemon\"]");
    assert_ne!(patched, cfg, "config splice no-oped: {cfg}");
    fs::write(&p, patched).unwrap();
    fs::create_dir_all(env.dev_b.claude().join("daemon")).unwrap();
    fs::write(env.dev_b.claude().join("daemon/marker.txt"), b"b-data").unwrap();
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_b, &["push"]);
    assert_eq!(c, 0, "{o}");

    // dev_a rewrites history: rekey rebuilds the manifest from A's locals,
    // which do NOT include daemon/ — the entry vanishes from the remote
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run_env(
        &env.dev_a,
        &["rekey"],
        &[("XSYNC_NEW_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 0, "rekey failed: {o}");

    // a cautious dev_b inspects first — the dry run consumes the mirror's
    // divergence signal, but the REAL pull must still re-anchor: without
    // that, stale anchors classify B's own daemon file as remote-deleted
    let (c, o) = run_env(
        &env.dev_b,
        &["pull", "--dry-run"],
        &[("XSYNC_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("would remove daemon/marker.txt"),
        "dry-run misclassifies B's own file as remote-deleted: {o}"
    );
    let (c, o) = run_env(&env.dev_b, &["pull"], &[("XSYNC_PASSPHRASE", "new-pass")]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("rewritten"),
        "real pull after a dry run lost the rewrite detection: {o}"
    );
    assert_eq!(
        fs::read(env.dev_b.claude().join("daemon/marker.txt")).unwrap(),
        b"b-data",
        "history rewrite + preceding dry-run deleted B's local file: {o}"
    );
}

#[test]
fn remote_deletion_propagates_with_backup() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    fs::create_dir_all(env.dev_a.claude().join("agents")).unwrap();
    fs::write(env.dev_a.claude().join("agents/foo.md"), b"# agent").unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(env.dev_b.claude().join("agents/foo.md").exists());

    fs::remove_file(env.dev_a.claude().join("agents/foo.md")).unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("removed agents/foo.md (deleted on remote; backup kept)"),
        "missing deletion report: {o}"
    );
    assert!(!env.dev_b.claude().join("agents/foo.md").exists());
    let backed_up = fs::read_dir(env.dev_b.home.path())
        .unwrap()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".claude.backup.")
                && e.path().join("agents/foo.md").is_file()
        });
    assert!(backed_up, "deleted file must be in a backup dir: {o}");
}

fn install_failing_hook(env: &TestEnv) {
    let h = env.bare.path().join("hooks/pre-receive");
    fs::write(&h, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&h, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn failed_push_never_poisons_later_syncs() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    // the remote rejects the next push AFTER the mirror commit is made
    install_failing_hook(&env);
    fs::write(
        env.dev_a.claude().join("settings.json"),
        b"{\"model\":\"v2\"}",
    )
    .unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_ne!(c, 0, "push must fail against the rejecting remote: {o}");
    fs::remove_file(env.bare.path().join("hooks/pre-receive")).unwrap();

    // the natural reaction the tool trains: pull, then push again
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        o.contains("✓ 1 synced"),
        "v2 must re-seal after the failed push (stranded mirror commit must not anchor): {o}"
    );

    // no false "history rewritten" warnings afterwards
    let (c, o) = run(&env.dev_a, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert!(
        !o.contains("rewritten"),
        "stranded anchor causes false rewrite warnings: {o}"
    );

    // and the edit actually reaches the fleet
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(
        fs::read(env.dev_b.claude().join("settings.json")).unwrap(),
        b"{\"model\":\"v2\"}",
        "v2 never propagated: {o}"
    );
}

#[test]
fn rekey_refuses_while_remote_entries_were_never_delivered() {
    let env = TestEnv::new();
    assert_eq!(env.init(&env.dev_a).0, 0);
    fs::create_dir_all(env.dev_a.claude().join("plans")).unwrap();
    fs::write(env.dev_a.claude().join("plans/n.jsonl"), b"{\"v\":1}\n").unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // A ships v2, then the object gets corrupted on the remote
    fs::write(env.dev_a.claude().join("plans/n.jsonl"), b"{\"v\":2}\n").unwrap();
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");
    corrupt_remote_objects(&env);

    // B's pull cannot stage v2 (skip, exit 1) but still anchors the commit
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 1, "expected skip exit: {o}");
    assert!(o.contains("skipping"), "{o}");

    // rekey would rebuild the manifest WITHOUT v2 and purge the history
    // that still holds it — refuse until the pull is clean
    let (c, o) = run_env(
        &env.dev_b,
        &["rekey"],
        &[("XSYNC_NEW_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 2, "rekey must refuse while entries are undelivered: {o}");
    assert!(
        o.contains("never delivered") || o.contains("undelivered"),
        "missing explanation: {o}"
    );

    // --force remains the escape hatch
    let (c, o) = run_env(
        &env.dev_b,
        &["rekey", "--force"],
        &[("XSYNC_NEW_PASSPHRASE", "new-pass")],
    );
    assert_eq!(c, 0, "forced rekey must proceed: {o}");
}

#[test]
fn mcp_both_modified_keeps_local_subtree_copy() {
    let env = TestEnv::new();
    fs::write(
        env.dev_a.home.path().join(".claude.json"),
        b"{\"mcpServers\":{\"a-server\":{\"command\":\"a\"}},\"other\":1}",
    )
    .unwrap();
    assert_eq!(env.init(&env.dev_a).0, 0);
    let (c, o) = run(&env.dev_a, &["push"]);
    assert_eq!(c, 0, "{o}");

    fs::write(
        env.dev_b.home.path().join(".claude.json"),
        b"{\"mcpServers\":{\"my-local-db\":{\"command\":\"b\"}}}",
    )
    .unwrap();
    assert_eq!(env.init(&env.dev_b).0, 0);
    let (c, o) = run(&env.dev_b, &["pull"]);
    assert_eq!(c, 0, "{o}");

    // remote subtree applies (same rule as file conflicts: remote wins live)…
    let live = fs::read_to_string(env.dev_b.home.path().join(".claude.json")).unwrap();
    assert!(
        live.contains("a-server"),
        "remote subtree not applied: {live}"
    );
    // …but the local servers must survive as a conflict copy, loudly
    assert!(
        o.contains("⚡ conflict on mcpServers"),
        "silent mcp replacement: {o}"
    );
    // settings.json (baseline fixture) + mcpServers = 2 conflicts total
    assert!(o.contains("⚡ 2 conflicts"), "conflict not counted: {o}");
    let copy = fs::read_dir(env.dev_b.claude())
        .unwrap()
        .flatten()
        .find(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("mcp-servers.xsync-conflict.")
        })
        .map(|e| fs::read_to_string(e.path()).unwrap());
    match copy {
        Some(body) => assert!(
            body.contains("my-local-db"),
            "conflict copy lacks the local servers: {body}"
        ),
        None => panic!("no mcp conflict copy written: {o}"),
    }
}

/// Corrupt every sealed object on the bare remote (clone → append junk →
/// commit → push) so subsequent staging of changed entries fails.
fn corrupt_remote_objects(env: &TestEnv) {
    let tmp = TempDir::new().unwrap();
    let clone = tmp.path().join("clone");
    let clone_s = clone.to_string_lossy().to_string();
    let out = Command::new("git")
        .args(["clone", &env.bare_url(), clone_s.as_str()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for e in fs::read_dir(clone.join("objects")).unwrap().flatten() {
        let mut bytes = fs::read(e.path()).unwrap();
        bytes.extend_from_slice(b"CORRUPT");
        fs::write(e.path(), bytes).unwrap();
    }
    for args in [
        vec!["-C", clone_s.as_str(), "add", "-A"],
        vec![
            "-C",
            clone_s.as_str(),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "tamper",
        ],
        vec!["-C", clone_s.as_str(), "push", "origin", "main"],
    ] {
        let out = Command::new("git").args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
