use crate::mapper::PathMapper;
use crate::transform::file::{
    classify, normalize_file, resolve_file_pull, split_lines, verify_resolve_line, FileKind,
    TransformOutcome,
};
use crate::transform::pathmatch::{resolve_text, ResolveMode};

/// Push gate: runs `normalize_file`, then enforces
/// - invariant A: verify-resolve re-splice of the transformed output is
///   byte-identical to the original (per line for JSON kinds), and
/// - invariant B: `normalize(resolve_pull(n)) == n` (pull fixpoint).
/// Any failure degrades the WHOLE file to `Verbatim` — corrupt data never
/// reaches the remote. Lines that failed to parse passed through verbatim in
/// `normalize_file` already (line-level fail-closed) and trivially satisfy A.
pub fn push_gate(rel_path: &str, original: &[u8], m: &PathMapper) -> TransformOutcome {
    let outcome = normalize_file(rel_path, original, m);
    let TransformOutcome::Transformed { data, spans } = outcome else { return outcome; };

    // Invariant A: byte round-trip via recorded span originals.
    match classify(rel_path) {
        FileKind::PlainText => {
            let text = String::from_utf8_lossy(&data);
            let records = spans.first().map(|v| v.as_slice()).unwrap_or(&[]);
            let restored = resolve_text(&text, m, ResolveMode::Verify(records));
            if restored.as_bytes() != original {
                return TransformOutcome::Verbatim { reason: "round-trip mismatch (text)".into() };
            }
        }
        FileKind::Jsonl | FileKind::Json => {
            let orig_lines = split_lines(original);
            let trans_lines = split_lines(&data);
            if orig_lines.len() != trans_lines.len() {
                return TransformOutcome::Verbatim { reason: "line count drift".into() };
            }
            for (idx, ((orig, trans), records)) in
                orig_lines.iter().zip(&trans_lines).zip(&spans).enumerate()
            {
                if trans == orig && records.is_empty() { continue; }
                match verify_resolve_line(trans, records, m) {
                    Some(restored) if restored == *orig => {}
                    _ => {
                        return TransformOutcome::Verbatim {
                            reason: format!("round-trip mismatch at line {}", idx),
                        };
                    }
                }
            }
        }
        FileKind::FileHistorySnapshot | FileKind::Unknown => unreachable!("already Verbatim"),
    }

    // Invariant B: pull fixpoint.
    let Ok(pulled) = resolve_file_pull(rel_path, &data, m) else {
        return TransformOutcome::Verbatim { reason: "pull resolve failed".into() };
    };
    match normalize_file(rel_path, &pulled, m) {
        TransformOutcome::Transformed { data: renorm, .. } if renorm == data => {}
        _ => return TransformOutcome::Verbatim { reason: "pull fixpoint mismatch".into() },
    }

    TransformOutcome::Transformed { data, spans }
}
