use crate::mapper::PathMapper;

#[derive(Debug, Clone)]
pub struct SpanRecord {
    pub original: String,
}

#[derive(Debug)]
pub struct NormalizedText {
    pub text: String,
    pub spans: Vec<SpanRecord>,
}

pub enum ResolveMode<'a> {
    Verify(&'a [SpanRecord]),
    Pull,
}

const ESC: &str = "${ESC}";

fn is_run_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(c, '_' | '-' | '.' | '/' | '\\' | '~' | '+' | '@')
        || !c.is_ascii()
}

/// Case-insensitive prefix match of `local` (with both separator variants)
/// at `pos`; returns matched length in bytes if boundary holds.
fn match_local_at(text: &str, pos: usize, local: &str) -> Option<usize> {
    let rest = &text[pos..];
    let mut ri = rest.chars();
    let mut matched = 0usize;
    for lc in local.chars() {
        let rc = ri.next()?;
        let sep_ok = matches!(lc, '/' | '\\') && matches!(rc, '/' | '\\');
        if !sep_ok && rc.to_lowercase().to_string() != lc.to_lowercase().to_string() {
            return None;
        }
        matched += rc.len_utf8();
    }
    // boundary: next char must be a separator, non-run char, or end
    match rest[matched..].chars().next() {
        None => Some(matched),
        Some('/' | '\\') => Some(matched),
        Some(c) if !is_run_char(c) => Some(matched),
        _ => None,
    }
}

pub fn normalize_text(input: &str, m: &PathMapper) -> NormalizedText {
    // Phase 1: injectivity escape
    let escaped = input.replace("${", ESC);
    // Phase 2: scan & replace
    let mut out = String::with_capacity(escaped.len());
    let mut spans = Vec::new();
    let mut i = 0;
    let mut prev: Option<char> = None;
    'outer: while i < escaped.len() {
        // Left boundary (spec §12.1): a run char directly before the match
        // start blocks it — except separators ('/', '\\'), which keep
        // file:/// URLs and \\?\ long-path prefixes translating.
        let left_ok = prev.is_none_or(|c| !is_run_char(c) || matches!(c, '/' | '\\'));
        if left_ok {
            for t in &m.tokens {
                if let Some(len) = match_local_at(&escaped, i, &t.local) {
                    let run_start = i + len;
                    let mut run_end = run_start;
                    for c in escaped[run_start..].chars() {
                        if is_run_char(c) {
                            run_end += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    // Original span must be recovered from pre-escape input: since
                    // ESC substitution only rewrites "${", and "${" cannot occur
                    // inside a matched local+run (run charset excludes '{'), the
                    // escaped slice equals the original slice here.
                    let original = escaped[i..run_end].to_string();
                    out.push_str(&format!("${{{}}}", t.name));
                    out.push_str(&escaped[run_start..run_end].replace('\\', "/"));
                    spans.push(SpanRecord { original });
                    prev = escaped[i..run_end].chars().last();
                    i = run_end;
                    continue 'outer;
                }
            }
        }
        let c = escaped[i..].chars().next().unwrap();
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    NormalizedText { text: out, spans }
}

/// Count the token occurrences `resolve_text` would consume in Verify mode.
/// Mirrors the resolve scan exactly: `${ESC}` prefixes are copied through and
/// never counted; registered `${NAME}` tokens skip their following run.
pub(crate) fn count_resolvable_tokens(input: &str, m: &PathMapper) -> usize {
    let mut count = 0usize;
    let mut i = 0;
    'outer: while i < input.len() {
        if input[i..].starts_with(ESC) {
            // copy-through, same as resolve_text
        } else if input[i..].starts_with("${") {
            for t in &m.tokens {
                let tok = format!("${{{}}}", t.name);
                if input[i..].starts_with(&tok) {
                    count += 1;
                    let run_start = i + tok.len();
                    let mut run_end = run_start;
                    for c in input[run_start..].chars() {
                        if is_run_char(c) {
                            run_end += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    i = run_end;
                    continue 'outer;
                }
            }
        }
        let c = input[i..].chars().next().unwrap();
        i += c.len_utf8();
    }
    count
}

pub fn resolve_text(input: &str, m: &PathMapper, mode: ResolveMode) -> String {
    let mut out = String::with_capacity(input.len());
    let mut span_idx = 0usize;
    let mut i = 0;
    'outer: while i < input.len() {
        if input[i..].starts_with(ESC) {
            // handled at the end — copy through for now
        } else if input[i..].starts_with("${") {
            for t in &m.tokens {
                let tok = format!("${{{}}}", t.name);
                if input[i..].starts_with(&tok) {
                    let run_start = i + tok.len();
                    let mut run_end = run_start;
                    for c in input[run_start..].chars() {
                        if is_run_char(c) {
                            run_end += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    match &mode {
                        ResolveMode::Verify(spans) => {
                            out.push_str(&spans[span_idx].original);
                            span_idx += 1;
                        }
                        ResolveMode::Pull => {
                            out.push_str(&t.local.replace('\\', "/"));
                            out.push_str(&input[run_start..run_end]); // already '/'
                        }
                    }
                    i = run_end;
                    continue 'outer;
                }
            }
        }
        let c = input[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out.replace(ESC, "${")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::PathMapper;
    use std::collections::BTreeMap;

    fn mac() -> PathMapper {
        PathMapper::new("/Users/woong", &BTreeMap::new()).unwrap()
    }
    fn win() -> PathMapper {
        PathMapper::new("C:\\Users\\Loki", &BTreeMap::new()).unwrap()
    }

    #[test]
    fn mac_path_tokenized() {
        let n = normalize_text("cd /Users/woong/workspace/foo now", &mac());
        assert_eq!(n.text, "cd ${HOME}/workspace/foo now");
        assert_eq!(n.spans[0].original, "/Users/woong/workspace/foo");
    }

    #[test]
    fn win_backslash_normalized_to_slash() {
        let n = normalize_text(r"C:\Users\Loki\workspace\foo", &win());
        assert_eq!(n.text, "${HOME}/workspace/foo");
    }

    #[test]
    fn case_insensitive_match_canonical_out() {
        let n = normalize_text(r"c:\users\loki\ws", &win());
        assert_eq!(n.text, "${HOME}/ws");
        assert_eq!(n.spans[0].original, r"c:\users\loki\ws"); // original case preserved
    }

    #[test]
    fn boundary_no_match_inside_longer_name() {
        let n = normalize_text("/Users/woongho/app", &mac());
        assert_eq!(n.text, "/Users/woongho/app");
        assert!(n.spans.is_empty());
    }

    #[test]
    fn other_os_home_untouched() {
        let n = normalize_text(r"log said C:\Users\Loki\x", &mac());
        assert_eq!(n.text, r"log said C:\Users\Loki\x");
    }

    #[test]
    fn literal_token_escaped_injective() {
        let n = normalize_text("run ${HOME}/bin and ${ESC} too", &mac());
        assert_eq!(n.text, "run ${ESC}HOME}/bin and ${ESC}ESC} too");
        // pull restores literals
        assert_eq!(
            resolve_text(&n.text, &mac(), ResolveMode::Pull),
            "run ${HOME}/bin and ${ESC} too"
        );
    }

    #[test]
    fn verify_roundtrip_byte_exact_mixed() {
        for (m, s) in [
            (mac(), "a /Users/woong/x b ${HOME} c /Users/woong"),
            (win(), r#"cwd C:\Users\Loki\p and c:/users/loki/q"#),
        ] {
            let n = normalize_text(s, &m);
            assert_eq!(resolve_text(&n.text, &m, ResolveMode::Verify(&n.spans)), s);
        }
    }

    #[test]
    fn pull_on_windows_emits_forward_slash() {
        let r = resolve_text("${HOME}/workspace/foo", &win(), ResolveMode::Pull);
        assert_eq!(r, "C:/Users/Loki/workspace/foo");
    }

    #[test]
    fn unknown_token_passes_through_in_content() {
        let r = resolve_text("echo ${PATH} and ${WORK}/x", &mac(), ResolveMode::Pull);
        assert_eq!(r, "echo ${PATH} and ${WORK}/x");
    }

    #[test]
    fn left_boundary_blocks_concatenated_home() {
        // macOS firmlink alias: previous char 'a' is a run char → no match
        let n = normalize_text("/System/Volumes/Data/Users/woong/ws", &mac());
        assert_eq!(n.text, "/System/Volumes/Data/Users/woong/ws");
        assert!(n.spans.is_empty());
    }

    #[test]
    fn left_boundary_allows_separator_prefix_urls() {
        // file:// URLs keep translating (previous char '/')
        let n = normalize_text("open file:///Users/woong/doc.md now", &mac());
        assert_eq!(n.text, "open file://${HOME}/doc.md now");
    }

    #[test]
    fn left_boundary_allows_longpath_prefix() {
        // \\?\ long-path prefix keeps translating (previous char '\')
        let n = normalize_text(r"\\?\C:\Users\Loki\ws", &win());
        assert_eq!(n.text, r"\\?\${HOME}/ws");
    }

    #[test]
    fn left_boundary_blocks_alnum_prefix_win() {
        let n = normalize_text(r"xC:\Users\Loki\ws", &win());
        assert_eq!(n.text, r"xC:\Users\Loki\ws");
        assert!(n.spans.is_empty());
    }

    #[test]
    fn korean_run_kept() {
        let n = normalize_text("/Users/woong/문서/메모.md", &mac());
        assert_eq!(n.text, "${HOME}/문서/메모.md");
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use crate::mapper::PathMapper;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    proptest! {
        #[test]
        fn invariants_a_and_b(seg in "[a-zA-Z0-9_./\\\\-]{0,24}", pre in "[ -~]{0,12}") {
            for home in ["/Users/woong", "C:\\Users\\Loki"] {
                let m = PathMapper::new(home, &BTreeMap::new()).unwrap();
                let input = format!("{pre} {home}{sep}{seg}", sep = if home.starts_with('/') {"/"} else {"\\"});
                let n = normalize_text(&input, &m);
                // A: byte round-trip
                prop_assert_eq!(resolve_text(&n.text, &m, ResolveMode::Verify(&n.spans)), input.clone());
                // B: fixpoint
                let pulled = resolve_text(&n.text, &m, ResolveMode::Pull);
                prop_assert_eq!(normalize_text(&pulled, &m).text, n.text);
            }
        }
    }
}
