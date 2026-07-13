use crate::mapper::PathMapper;
use crate::transform::dirkey::UnmappedToken;
use crate::transform::json_spans::{
    decode_json_string, encode_json_string, string_spans, LexError,
};
use crate::transform::pathmatch::{normalize_text, resolve_text, ResolveMode, SpanRecord};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileKind {
    Jsonl,
    Json,
    PlainText,
    FileHistorySnapshot,
    Unknown,
}

pub fn classify(rel_path: &str) -> FileKind {
    let rel = rel_path.replace('\\', "/");
    if rel.starts_with("file-history/") && !rel.ends_with(".json") {
        return FileKind::FileHistorySnapshot;
    }
    if rel.ends_with(".jsonl") {
        return FileKind::Jsonl;
    }
    if rel.ends_with(".json") {
        return FileKind::Json;
    }
    if rel.ends_with(".md") || rel.ends_with(".txt") {
        return FileKind::PlainText;
    }
    FileKind::Unknown
}

#[derive(Debug)]
pub enum TransformOutcome {
    Transformed {
        data: Vec<u8>,
        spans: Vec<Vec<SpanRecord>>,
    }, // per line
    Verbatim {
        reason: String,
    },
}

/// Splice one JSONL/JSON line: rewrite only the string tokens whose decoded
/// text changes under `normalize_text`; every other byte is preserved.
fn transform_line(line: &[u8], m: &PathMapper) -> Result<(Vec<u8>, Vec<SpanRecord>), LexError> {
    let spans = string_spans(line)?;
    let mut out = Vec::with_capacity(line.len());
    let mut records = Vec::new();
    let mut cursor = 0usize;
    for sp in spans {
        out.extend_from_slice(&line[cursor..sp.start]);
        let decoded = decode_json_string(&line[sp.start..sp.end])?;
        let n = normalize_text(&decoded, m);
        if n.text == decoded {
            out.extend_from_slice(&line[sp.start..sp.end]); // untouched bytes
        } else {
            out.extend(encode_json_string(&n.text));
            records.extend(n.spans);
        }
        cursor = sp.end;
    }
    out.extend_from_slice(&line[cursor..]);
    Ok((out, records))
}

pub fn normalize_file(rel_path: &str, data: &[u8], m: &PathMapper) -> TransformOutcome {
    match classify(rel_path) {
        FileKind::FileHistorySnapshot => TransformOutcome::Verbatim {
            reason: "file-history snapshot (undo bytes sacred)".into(),
        },
        FileKind::Unknown => TransformOutcome::Verbatim {
            reason: "unknown format".into(),
        },
        FileKind::PlainText => {
            let Ok(text) = std::str::from_utf8(data) else {
                return TransformOutcome::Verbatim {
                    reason: "non-utf8 text".into(),
                };
            };
            let n = normalize_text(text, m);
            TransformOutcome::Transformed {
                data: n.text.into_bytes(),
                spans: vec![n.spans],
            }
        }
        FileKind::Jsonl | FileKind::Json => {
            let mut out = Vec::with_capacity(data.len());
            let mut spans = Vec::new();
            for line in split_lines(data) {
                match transform_line(line, m) {
                    Ok((bytes, records)) => {
                        out.extend(bytes);
                        spans.push(records);
                    }
                    Err(_) => {
                        out.extend_from_slice(line);
                        spans.push(Vec::new());
                    } // line-level fail-closed
                }
            }
            TransformOutcome::Transformed { data: out, spans }
        }
    }
}

pub fn resolve_file_pull(
    rel_path: &str,
    data: &[u8],
    m: &PathMapper,
) -> Result<Vec<u8>, UnmappedToken> {
    match classify(rel_path) {
        FileKind::FileHistorySnapshot | FileKind::Unknown => Ok(data.to_vec()),
        FileKind::PlainText => {
            let Ok(text) = std::str::from_utf8(data) else {
                return Ok(data.to_vec());
            };
            Ok(resolve_text(text, m, ResolveMode::Pull).into_bytes())
        }
        FileKind::Jsonl | FileKind::Json => {
            let mut out = Vec::with_capacity(data.len());
            for line in split_lines(data) {
                match resolve_line_pull(line, m) {
                    Some(bytes) => out.extend(bytes),
                    None => out.extend_from_slice(line),
                }
            }
            Ok(out)
        }
    }
}

fn resolve_line_pull(line: &[u8], m: &PathMapper) -> Option<Vec<u8>> {
    let spans = string_spans(line).ok()?;
    let mut out = Vec::with_capacity(line.len());
    let mut cursor = 0usize;
    for sp in spans {
        out.extend_from_slice(&line[cursor..sp.start]);
        let raw = &line[sp.start..sp.end];
        let decoded = decode_json_string(raw).ok()?;
        if decoded.contains("${") {
            let resolved = resolve_text(&decoded, m, ResolveMode::Pull);
            if resolved != decoded {
                out.extend(encode_json_string(&resolved));
            } else {
                out.extend_from_slice(raw);
            }
        } else {
            out.extend_from_slice(raw);
        }
        cursor = sp.end;
    }
    out.extend_from_slice(&line[cursor..]);
    Some(out)
}

/// Re-splice recorded original spans into a transformed JSON line (invariant A).
/// Returns None on any accounting mismatch.
pub(crate) fn verify_resolve_line(
    trans: &[u8],
    records: &[SpanRecord],
    m: &PathMapper,
) -> Option<Vec<u8>> {
    use crate::transform::pathmatch::count_resolvable_tokens;
    let spans = string_spans(trans).ok()?;
    let mut out = Vec::with_capacity(trans.len());
    let mut cursor = 0usize;
    let mut idx = 0usize;
    for sp in spans {
        out.extend_from_slice(&trans[cursor..sp.start]);
        let raw = &trans[sp.start..sp.end];
        let decoded = decode_json_string(raw).ok()?;
        if decoded.contains("${") {
            let cnt = count_resolvable_tokens(&decoded, m);
            if idx + cnt > records.len() {
                return None;
            }
            let resolved = resolve_text(&decoded, m, ResolveMode::Verify(&records[idx..idx + cnt]));
            idx += cnt;
            if resolved != decoded {
                out.extend(encode_json_string(&resolved));
            } else {
                out.extend_from_slice(raw);
            }
        } else {
            out.extend_from_slice(raw);
        }
        cursor = sp.end;
    }
    out.extend_from_slice(&trans[cursor..]);
    (idx == records.len()).then_some(out)
}

pub(crate) fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    if data.is_empty() {
        return vec![];
    }
    data.split_inclusive(|&b| b == b'\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::PathMapper;
    use crate::verify::push_gate;
    use std::collections::BTreeMap;

    fn mac() -> PathMapper {
        PathMapper::new("/Users/woong", &BTreeMap::new()).unwrap()
    }
    fn win() -> PathMapper {
        PathMapper::new("C:\\Users\\Loki", &BTreeMap::new()).unwrap()
    }

    #[test]
    fn jsonl_cwd_and_key_paths_rewritten() {
        let m = mac();
        let line = br#"{"cwd":"/Users/woong/ws/app","trackedFileBackups":{"/Users/woong/ws/app/src/main.rs":"h1"}}"#;
        match normalize_file("projects/x/s.jsonl", line, &m) {
            TransformOutcome::Transformed { data, .. } => {
                let s = String::from_utf8(data).unwrap();
                assert!(s.contains(r#""cwd":"${HOME}/ws/app""#));
                assert!(s.contains(r#""${HOME}/ws/app/src/main.rs""#));
            }
            _ => panic!("expected transform"),
        }
    }

    #[test]
    fn truncated_last_line_passes_verbatim_line_level() {
        let m = mac();
        let data = b"{\"cwd\":\"/Users/woong/a\"}\n{\"cwd\":\"/Users/wo";
        match normalize_file("projects/x/s.jsonl", data, &m) {
            TransformOutcome::Transformed { data: out, .. } => {
                let s = String::from_utf8(out).unwrap();
                assert!(s.starts_with("{\"cwd\":\"${HOME}/a\"}\n"));
                assert!(s.ends_with("{\"cwd\":\"/Users/wo")); // untouched
            }
            _ => panic!(),
        }
    }

    #[test]
    fn file_history_snapshot_always_verbatim() {
        let m = mac();
        let r = normalize_file("file-history/abc/1.md", b"path /Users/woong/x", &m);
        assert!(matches!(r, TransformOutcome::Verbatim { .. }));
    }

    #[test]
    fn push_gate_roundtrips_windows_content() {
        let m = win();
        let line = br#"{"cwd":"C:\\Users\\Loki\\ws\\app"}"#;
        // must NOT degrade to verbatim (the C1 regression test)
        assert!(matches!(
            push_gate("projects/x/s.jsonl", line, &m),
            TransformOutcome::Transformed { .. }
        ));
    }

    #[test]
    fn pull_resolve_emits_slash_form_in_json() {
        let m = win();
        let portable = br#"{"cwd":"${HOME}/ws/app"}"#;
        let out = resolve_file_pull("projects/x/s.jsonl", portable, &m).unwrap();
        assert_eq!(out, br#"{"cwd":"C:/Users/Loki/ws/app"}"#.to_vec());
    }
}
