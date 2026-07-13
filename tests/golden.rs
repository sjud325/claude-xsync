//! Golden cross-OS snapshot tests. Regenerate committed outputs with
//! `UPDATE_GOLDEN=1 cargo test --test golden` and INSPECT them manually
//! (cwd tokenized, literal ${..} escaped, Korean preserved) before committing.
use claude_xsync::mapper::PathMapper;
use claude_xsync::transform::file::{normalize_file, resolve_file_pull, TransformOutcome};
use std::collections::BTreeMap;
use std::fs;

fn mac() -> PathMapper {
    PathMapper::new("/Users/woong", &BTreeMap::new()).unwrap()
}
fn win() -> PathMapper {
    PathMapper::new("C:\\Users\\Loki", &BTreeMap::new()).unwrap()
}

fn check_or_update(path: &str, actual: &[u8]) {
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        fs::write(path, actual).unwrap();
        return;
    }
    let expected = fs::read(path)
        .unwrap_or_else(|_| panic!("missing golden {path}; run UPDATE_GOLDEN=1 cargo test --test golden"));
    assert_eq!(
        String::from_utf8_lossy(&expected),
        String::from_utf8_lossy(actual),
        "golden mismatch: {path}"
    );
}

fn normalize_or_die(src: &[u8], m: &PathMapper) -> Vec<u8> {
    match normalize_file("projects/x/s.jsonl", src, m) {
        TransformOutcome::Transformed { data, .. } => data,
        TransformOutcome::Verbatim { reason } => panic!("fixture degraded to verbatim: {reason}"),
    }
}

#[test]
fn golden_mac_session_both_directions() {
    let src = fs::read("tests/fixtures/mac-session.jsonl").unwrap();
    let portable = normalize_or_die(&src, &mac());
    check_or_update("tests/fixtures/mac-session.portable.jsonl", &portable);
    let on_win = resolve_file_pull("projects/x/s.jsonl", &portable, &win()).unwrap();
    check_or_update("tests/fixtures/mac-session.on-win.jsonl", &on_win);
}

#[test]
fn golden_win_session_both_directions() {
    let src = fs::read("tests/fixtures/win-session.jsonl").unwrap();
    let portable = normalize_or_die(&src, &win());
    check_or_update("tests/fixtures/win-session.portable.jsonl", &portable);
    let on_mac = resolve_file_pull("projects/x/s.jsonl", &portable, &mac()).unwrap();
    check_or_update("tests/fixtures/win-session.on-mac.jsonl", &on_mac);
}
