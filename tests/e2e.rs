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
    assert!(v["cwd"].as_str().unwrap().ends_with("/ws/app"), "{v}");

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
